// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::agent::{AgentCreator, AgentError, Context, Invocation, Lifespan, Payload};
use crate::base::SettingType;
use crate::message::base::{Audience, MessengerType};
use crate::service;
use crate::service_context::ServiceContext;
use anyhow::{format_err, Context as _, Error};
use std::collections::HashSet;
use std::rc::Rc;

/// Authority provides the ability to execute agents sequentially or simultaneously for a given
/// stage.
pub(crate) struct Authority {
    // This is a list of pairs of debug ids and agent addresses.
    agent_signatures: Vec<(&'static str, service::message::Signature)>,
    // Factory passed to agents for communicating with the service.
    delegate: service::message::Delegate,
    // Messenger
    messenger: service::message::Messenger,
    // Available components
    available_components: HashSet<SettingType>,
}

impl Authority {
    pub(crate) async fn create(
        delegate: service::message::Delegate,
        available_components: HashSet<SettingType>,
    ) -> Result<Authority, Error> {
        let (client, _) = delegate
            .create(MessengerType::Unbound)
            .await
            .map_err(|_| anyhow::format_err!("could not create agent messenger for authority"))?;

        Ok(Authority {
            agent_signatures: Vec::new(),
            delegate,
            messenger: client,
            available_components,
        })
    }

    pub(crate) async fn register(&mut self, creator: AgentCreator) {
        let agent_receptor = self
            .delegate
            .create(MessengerType::Unbound)
            .await
            .expect("agent receptor should be created")
            .1;
        let signature = agent_receptor.get_signature();
        let context =
            Context::new(agent_receptor, self.delegate.clone(), self.available_components.clone())
                .await;

        creator.create(context).await;

        self.agent_signatures.push((creator.debug_id, signature));
    }

    /// Invokes each registered agent for a given lifespan. If sequential is true,
    /// invocations will only proceed to the next agent once the current
    /// invocation has been successfully acknowledged. When sequential is false,
    /// agents will receive their invocations without waiting. However, the
    /// overall completion (signaled through the receiver returned by the method),
    /// will not return until all invocations have been acknowledged.
    pub(crate) async fn execute_lifespan(
        &self,
        lifespan: Lifespan,
        service_context: Rc<ServiceContext>,
        sequential: bool,
    ) -> Result<(), Error> {
        let mut pending_receptors = Vec::new();

        for &(debug_id, signature) in &self.agent_signatures {
            let mut receptor = self.messenger.message(
                Payload::Invocation(Invocation {
                    lifespan,
                    service_context: Rc::clone(&service_context),
                })
                .into(),
                Audience::Messenger(signature),
            );

            if sequential {
                let result = process_payload(debug_id, receptor.next_of::<Payload>().await);
                #[allow(clippy::question_mark)]
                if result.is_err() {
                    return result;
                }
            } else {
                pending_receptors.push((debug_id, receptor));
            }
        }

        // Pending acks should only be present for non sequential execution. In
        // this case wait for each to complete.
        for (debug_id, mut receptor) in pending_receptors {
            let result = process_payload(debug_id, receptor.next_of::<Payload>().await);
            #[allow(clippy::question_mark)]
            if result.is_err() {
                return result;
            }
        }

        Ok(())
    }
}

fn process_payload(
    debug_id: &str,
    payload: Result<(Payload, service::message::MessageClient), Error>,
) -> Result<(), Error> {
    match payload {
        Ok((Payload::Complete(Ok(_) | Err(AgentError::UnhandledLifespan)), _)) => Ok(()),
        Ok((Payload::Complete(result), _)) => {
            result.with_context(|| format!("Invocation failed for {debug_id:?}"))
        }
        Ok(_) => Err(format_err!("Unexpected result for {:?}", debug_id)),
        Err(e) => Err(e).with_context(|| format!("Invocation failed {debug_id:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::message_hub::MessageHub;
    use assert_matches::assert_matches;
    use fuchsia_async as fasync;

    fn create(context: Context) -> futures::future::LocalBoxFuture<'static, ()> {
        Box::pin(async move {
            let _ = &context;
            let mut receptor = context.receptor;
            fasync::Task::local(async move {
                while let Ok((Payload::Invocation(_), client)) = receptor.next_of::<Payload>().await
                {
                    let _ =
                        client.reply(Payload::Complete(Err(AgentError::UnexpectedError)).into());
                }
            })
            .detach();
        })
    }

    #[fasync::run_until_stalled(test)]
    async fn test_log() {
        let delegate = MessageHub::create();
        let mut authority = Authority::create(delegate, HashSet::new())
            .await
            .expect("Should be able to create authority");
        authority.register(crate::create_agent!(test_agent, create)).await;
        let result = authority
            .execute_lifespan(
                Lifespan::Initialization,
                Rc::new(ServiceContext::new(None, None)),
                false,
            )
            .await;
        assert_matches!(result, Err(e) if format!("{e:?}").contains("test_agent"));
    }
}
