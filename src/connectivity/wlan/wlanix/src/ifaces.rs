// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::bss_scorer::BssScorer;
use crate::security::{get_authenticator, Credential};
use anyhow::{bail, format_err, Context, Error};
use async_trait::async_trait;
use fidl::endpoints::create_proxy;
use fuchsia_async::TimeoutExt;
use fuchsia_sync::Mutex;
use futures::channel::oneshot;
use futures::{select, FutureExt, TryStreamExt};
use ieee80211::Bssid;
use std::collections::HashMap;
use std::convert::TryFrom;
use std::sync::Arc;
use tracing::info;
use wlan_common::bss::BssDescription;
use wlan_common::scan::Compatibility;
use {
    fidl_fuchsia_wlan_common as fidl_common,
    fidl_fuchsia_wlan_device_service as fidl_device_service,
    fidl_fuchsia_wlan_ieee80211 as fidl_ieee80211, fidl_fuchsia_wlan_internal as fidl_internal,
    fidl_fuchsia_wlan_sme as fidl_sme,
};

#[async_trait]
pub(crate) trait IfaceManager: Send + Sync {
    type Client: ClientIface;

    async fn list_phys(&self) -> Result<Vec<u16>, Error>;
    fn list_ifaces(&self) -> Vec<u16>;
    async fn get_country(&self, phy_id: u16) -> Result<[u8; 2], Error>;
    async fn query_iface(
        &self,
        iface_id: u16,
    ) -> Result<fidl_device_service::QueryIfaceResponse, Error>;
    async fn create_client_iface(&self, phy_id: u16) -> Result<u16, Error>;
    async fn get_client_iface(&self, iface_id: u16) -> Result<Arc<Self::Client>, Error>;
    async fn destroy_iface(&self, iface_id: u16) -> Result<(), Error>;
}

pub struct DeviceMonitorIfaceManager {
    monitor_svc: fidl_device_service::DeviceMonitorProxy,
    ifaces: Mutex<HashMap<u16, Arc<SmeClientIface>>>,
}

impl DeviceMonitorIfaceManager {
    pub fn new() -> Result<Self, Error> {
        let monitor_svc = fuchsia_component::client::connect_to_protocol::<
            fidl_device_service::DeviceMonitorMarker,
        >()
        .context("failed to connect to device monitor")?;
        Ok(Self { monitor_svc, ifaces: Mutex::new(HashMap::new()) })
    }

    pub fn clone_device_monitor_svc(&self) -> fidl_device_service::DeviceMonitorProxy {
        self.monitor_svc.clone()
    }
}

#[async_trait]
impl IfaceManager for DeviceMonitorIfaceManager {
    type Client = SmeClientIface;

    async fn list_phys(&self) -> Result<Vec<u16>, Error> {
        self.monitor_svc.list_phys().await.map_err(Into::into)
    }

    fn list_ifaces(&self) -> Vec<u16> {
        self.ifaces.lock().keys().cloned().collect::<Vec<_>>()
    }

    async fn get_country(&self, phy_id: u16) -> Result<[u8; 2], Error> {
        let result = self.monitor_svc.get_country(phy_id).await.map_err(Into::<Error>::into)?;
        match result {
            Ok(get_country_response) => Ok(get_country_response.alpha2),
            Err(e) => match zx::Status::ok(e) {
                Err(e) => Err(e.into()),
                Ok(()) => Err(format_err!("get_country returned error with ok status")),
            },
        }
    }

    async fn query_iface(
        &self,
        iface_id: u16,
    ) -> Result<fidl_device_service::QueryIfaceResponse, Error> {
        self.monitor_svc
            .query_iface(iface_id)
            .await?
            .map_err(zx::Status::from_raw)
            .context("Could not query iface info")
    }

    async fn create_client_iface(&self, phy_id: u16) -> Result<u16, Error> {
        // TODO(b/298030838): Remove unmanaged iface support when wlanix is the sole config path.
        let existing_iface_ids = self.monitor_svc.list_ifaces().await?;
        let mut unmanaged_iface_id = None;
        for iface_id in existing_iface_ids {
            if !self.ifaces.lock().contains_key(&iface_id) {
                let iface = self.query_iface(iface_id).await?;
                if iface.role == fidl_common::WlanMacRole::Client {
                    info!("Found existing client iface -- skipping iface creation");
                    unmanaged_iface_id = Some(iface_id);
                    break;
                }
            }
        }
        let (iface_id, wlanix_provisioned) = match unmanaged_iface_id {
            Some(id) => (id, false),
            None => {
                let (status, response) = self
                    .monitor_svc
                    .create_iface(&fidl_device_service::CreateIfaceRequest {
                        phy_id,
                        role: fidl_fuchsia_wlan_common::WlanMacRole::Client,
                        // TODO(b/322060085): Determine if we need to populate this and how.
                        sta_addr: [0u8; 6],
                    })
                    .await?;
                zx::Status::ok(status)?;
                (
                    response
                        .ok_or_else(|| format_err!("Did not receive a CreateIfaceResponse"))?
                        .iface_id,
                    true,
                )
            }
        };

        let (sme_proxy, server) = create_proxy::<fidl_sme::ClientSmeMarker>()?;
        self.monitor_svc.get_client_sme(iface_id, server).await?.map_err(zx::Status::from_raw)?;
        let mut iface = SmeClientIface::new(sme_proxy);
        iface.wlanix_provisioned = wlanix_provisioned;
        let _ = self.ifaces.lock().insert(iface_id, Arc::new(iface));
        Ok(iface_id)
    }

    async fn get_client_iface(&self, iface_id: u16) -> Result<Arc<SmeClientIface>, Error> {
        match self.ifaces.lock().get(&iface_id) {
            Some(iface) => Ok(iface.clone()),
            None => Err(format_err!("Requested unknown iface {}", iface_id)),
        }
    }

    async fn destroy_iface(&self, iface_id: u16) -> Result<(), Error> {
        // TODO(b/298030838): Remove unmanaged iface support when wlanix is the sole config path.
        let removed_iface = self.ifaces.lock().remove(&iface_id);
        if let Some(iface) = removed_iface {
            if iface.wlanix_provisioned {
                let status = self
                    .monitor_svc
                    .destroy_iface(&fidl_device_service::DestroyIfaceRequest { iface_id })
                    .await?;
                zx::Status::ok(status).map_err(|e| e.into())
            } else {
                info!("Iface {} was not provisioned by wlanix. Skipping destruction.", iface_id);
                Ok(())
            }
        } else {
            Ok(())
        }
    }
}

pub(crate) struct ConnectSuccess {
    pub bss: Box<BssDescription>,
    pub transaction_stream: fidl_sme::ConnectTransactionEventStream,
}

#[derive(Debug)]
pub(crate) struct ConnectFail {
    pub bss: Box<BssDescription>,
    pub status_code: fidl_ieee80211::StatusCode,
    pub timed_out: bool,
}

#[derive(Debug)]
pub(crate) enum ConnectResult {
    Success(ConnectSuccess),
    Fail(ConnectFail),
}

impl std::fmt::Debug for ConnectSuccess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> Result<(), std::fmt::Error> {
        write!(f, "ConnectSuccess {{ ssid: {:?}, bssid: {:?} }}", self.bss.ssid, self.bss.bssid)
    }
}

#[derive(Debug)]
pub(crate) enum ScanEnd {
    Complete,
    Cancelled,
}

#[async_trait]
pub(crate) trait ClientIface: Sync + Send {
    async fn trigger_scan(&self) -> Result<ScanEnd, Error>;
    async fn abort_scan(&self) -> Result<(), Error>;
    fn get_last_scan_results(&self) -> Vec<fidl_sme::ScanResult>;
    async fn connect_to_network(
        &self,
        ssid: &[u8],
        passphrase: Option<Vec<u8>>,
        requested_bssid: Option<Bssid>,
    ) -> Result<ConnectResult, Error>;
    async fn disconnect(&self) -> Result<(), Error>;
    fn get_connected_network_rssi(&self) -> Option<i8>;

    fn on_disconnect(&self, info: &fidl_sme::DisconnectSource);
    fn on_signal_report(&self, ind: fidl_internal::SignalReportIndication);
}

#[derive(Debug)]
pub(crate) struct SmeClientIface {
    sme_proxy: fidl_sme::ClientSmeProxy,
    last_scan_results: Arc<Mutex<Vec<fidl_sme::ScanResult>>>,
    scan_abort_signal: Arc<Mutex<Option<oneshot::Sender<()>>>>,
    connected_network_rssi: Arc<Mutex<Option<i8>>>,
    // TODO(b/298030838): Remove unmanaged iface support when wlanix is the sole config path.
    wlanix_provisioned: bool,
    bss_scorer: BssScorer,
}

impl SmeClientIface {
    fn new(sme_proxy: fidl_sme::ClientSmeProxy) -> Self {
        SmeClientIface {
            sme_proxy,
            last_scan_results: Arc::new(Mutex::new(vec![])),
            scan_abort_signal: Arc::new(Mutex::new(None)),
            connected_network_rssi: Arc::new(Mutex::new(None)),
            wlanix_provisioned: true,
            bss_scorer: BssScorer::new(),
        }
    }
}

#[async_trait]
impl ClientIface for SmeClientIface {
    async fn trigger_scan(&self) -> Result<ScanEnd, Error> {
        let scan_request = fidl_sme::ScanRequest::Passive(fidl_sme::PassiveScanRequest);
        let (abort_sender, mut abort_receiver) = oneshot::channel();
        self.scan_abort_signal.lock().replace(abort_sender);
        let mut fut = self.sme_proxy.scan(&scan_request);
        select! {
            scan_results = fut => {
                let scan_result_vmo = scan_results
                    .context("Failed to request scan")?
                    .map_err(|e| format_err!("Scan ended with error: {:?}", e))?;
                info!("Got scan results from SME.");
                *self.last_scan_results.lock() = wlan_common::scan::read_vmo(scan_result_vmo)?;
                self.scan_abort_signal.lock().take();
                Ok(ScanEnd::Complete)
            }
            _ = abort_receiver => {
                info!("Scan cancelled, ignoring results from SME.");
                Ok(ScanEnd::Cancelled)
            }
        }
    }

    async fn abort_scan(&self) -> Result<(), Error> {
        // TODO(https://fxbug.dev/42079074): Actually pipe this call down to SME.
        if let Some(sender) = self.scan_abort_signal.lock().take() {
            sender.send(()).map_err(|_| format_err!("Unable to send scan abort signal"))
        } else {
            Ok(())
        }
    }

    fn get_last_scan_results(&self) -> Vec<fidl_sme::ScanResult> {
        self.last_scan_results.lock().clone()
    }

    async fn connect_to_network(
        &self,
        ssid: &[u8],
        passphrase: Option<Vec<u8>>,
        bssid: Option<Bssid>,
    ) -> Result<ConnectResult, Error> {
        let last_scan_results = self.last_scan_results.lock().clone();
        info!("Checking for network in last scan: {} access points", last_scan_results.len());
        let mut scan_results = last_scan_results
            .iter()
            .filter_map(|r| {
                let bss_description = BssDescription::try_from(r.bss_description.clone());
                let compatibility =
                    r.compatibility.clone().map(|c| Compatibility::try_from(*c)).transpose();
                match (bss_description, compatibility) {
                    (Ok(bss_description), Ok(compatibility)) if bss_description.ssid == *ssid => {
                        match bssid {
                            Some(bssid) if bss_description.bssid != bssid => None,
                            _ => Some((bss_description, compatibility)),
                        }
                    }
                    _ => None,
                }
            })
            .collect::<Vec<_>>();
        scan_results.sort_by_key(|(bss_description, _)| self.bss_scorer.score_bss(bss_description));

        let (bss_description, compatibility) = match scan_results.pop() {
            Some(scan_result) => scan_result,
            None => bail!("Requested network not found"),
        };

        let credential = passphrase.map(|p| Credential::Password(p)).unwrap_or(Credential::None);
        let authenticator =
            match get_authenticator(bss_description.bssid, compatibility, &credential) {
                Some(authenticator) => authenticator,
                None => bail!("Failed to create authenticator for requested network. Unsupported security type, channel, or data rate."),
            };

        info!("Selected BSS to connect to");
        let (connect_txn, remote) = create_proxy()?;
        let bssid = bss_description.bssid;
        let connect_req = fidl_sme::ConnectRequest {
            ssid: bss_description.ssid.clone().into(),
            bss_description: bss_description.clone().into(),
            multiple_bss_candidates: false,
            authentication: authenticator.into(),
            deprecated_scan_type: fidl_common::ScanType::Passive,
        };
        self.sme_proxy.connect(&connect_req, Some(remote))?;

        info!("Waiting for connect result from SME");
        let mut stream = connect_txn.take_event_stream();
        let (sme_result, timed_out) = wait_for_connect_result(&mut stream)
            .map(|res| (res, false))
            .on_timeout(zx::MonotonicDuration::from_seconds(30), || {
                (
                    Ok(fidl_sme::ConnectResult {
                        code: fidl_ieee80211::StatusCode::RejectedSequenceTimeout,
                        is_credential_rejected: false,
                        is_reconnect: false,
                    }),
                    true,
                )
            })
            .await;
        let sme_result = sme_result?;

        info!("Received connect result from SME: {:?}", sme_result);
        if sme_result.code == fidl_ieee80211::StatusCode::Success {
            Ok(ConnectResult::Success(ConnectSuccess {
                bss: Box::new(bss_description),
                transaction_stream: stream,
            }))
        } else {
            self.bss_scorer.report_connect_failure(bssid, &sme_result);
            Ok(ConnectResult::Fail(ConnectFail {
                bss: Box::new(bss_description),
                status_code: sme_result.code,
                timed_out,
            }))
        }
    }

    async fn disconnect(&self) -> Result<(), Error> {
        // Note: we are forwarding disconnect request to SME, but we are not clearing
        //       any connected network state here because we expect this struct's `on_disconnect`
        //       to be called later.
        self.sme_proxy.disconnect(fidl_sme::UserDisconnectReason::Unknown).await?;
        Ok(())
    }

    fn get_connected_network_rssi(&self) -> Option<i8> {
        *self.connected_network_rssi.lock()
    }

    fn on_disconnect(&self, _info: &fidl_sme::DisconnectSource) {
        self.connected_network_rssi.lock().take();
    }

    fn on_signal_report(&self, ind: fidl_internal::SignalReportIndication) {
        let _prev = self.connected_network_rssi.lock().replace(ind.rssi_dbm);
    }
}

/// Wait until stream returns an OnConnectResult event or None. Ignore other event types.
/// TODO(https://fxbug.dev/42084621): Function taken from wlancfg. Dedupe later.
async fn wait_for_connect_result(
    stream: &mut fidl_sme::ConnectTransactionEventStream,
) -> Result<fidl_sme::ConnectResult, Error> {
    loop {
        let stream_fut = stream.try_next();
        match stream_fut
            .await
            .map_err(|e| format_err!("Failed to receive connect result from sme: {:?}", e))?
        {
            Some(fidl_sme::ConnectTransactionEvent::OnConnectResult { result }) => {
                return Ok(result)
            }
            Some(other) => {
                info!(
                    "Expected ConnectTransactionEvent::OnConnectResult, got {}. Ignoring.",
                    connect_txn_event_name(&other)
                );
            }
            None => {
                return Err(format_err!(
                    "Server closed the ConnectTransaction channel before sending a response"
                ));
            }
        };
    }
}

fn connect_txn_event_name(event: &fidl_sme::ConnectTransactionEvent) -> &'static str {
    match event {
        fidl_sme::ConnectTransactionEvent::OnConnectResult { .. } => "OnConnectResult",
        fidl_sme::ConnectTransactionEvent::OnRoamResult { .. } => "OnRoamResult",
        fidl_sme::ConnectTransactionEvent::OnDisconnect { .. } => "OnDisconnect",
        fidl_sme::ConnectTransactionEvent::OnSignalReport { .. } => "OnSignalReport",
        fidl_sme::ConnectTransactionEvent::OnChannelSwitched { .. } => "OnChannelSwitched",
    }
}

#[cfg(test)]
pub mod test_utils {
    use super::*;
    use fidl_fuchsia_wlan_internal as fidl_internal;
    use ieee80211::{MacAddrBytes, Ssid};
    use rand::Rng as _;
    use wlan_common::random_bss_description;

    pub static FAKE_IFACE_RESPONSE: fidl_device_service::QueryIfaceResponse =
        fidl_device_service::QueryIfaceResponse {
            role: fidl_fuchsia_wlan_common::WlanMacRole::Client,
            id: 1,
            phy_id: 10,
            phy_assigned_id: 100,
            sta_addr: [1, 2, 3, 4, 5, 6],
        };

    pub fn fake_scan_result() -> fidl_sme::ScanResult {
        fidl_sme::ScanResult {
            compatibility: None,
            timestamp_nanos: 1000,
            bss_description: fidl_common::BssDescription {
                bssid: [1, 2, 3, 4, 5, 6],
                bss_type: fidl_common::BssType::Infrastructure,
                beacon_period: 100,
                capability_info: 123,
                ies: vec![1, 2, 3, 2, 1],
                channel: fidl_common::WlanChannel {
                    primary: 1,
                    cbw: fidl_common::ChannelBandwidth::Cbw20,
                    secondary80: 0,
                },
                rssi_dbm: -40,
                snr_db: -50,
            },
        }
    }

    #[derive(Debug, Clone)]
    pub enum ClientIfaceCall {
        TriggerScan,
        AbortScan,
        GetLastScanResults,
        ConnectToNetwork { ssid: Vec<u8>, passphrase: Option<Vec<u8>>, bssid: Option<Bssid> },
        Disconnect,
        GetConnectedNetworkRssi,
        OnDisconnect { info: fidl_sme::DisconnectSource },
        OnSignalReport { ind: fidl_internal::SignalReportIndication },
    }

    pub struct TestClientIface {
        pub transaction_handle: Mutex<Option<fidl_sme::ConnectTransactionControlHandle>>,
        scan_end_receiver: Mutex<Option<oneshot::Receiver<Result<ScanEnd, Error>>>>,
        pub calls: Arc<Mutex<Vec<ClientIfaceCall>>>,
        pub connect_success: Mutex<bool>,
    }

    impl TestClientIface {
        pub fn new() -> Self {
            Self {
                transaction_handle: Mutex::new(None),
                scan_end_receiver: Mutex::new(None),
                calls: Arc::new(Mutex::new(vec![])),
                connect_success: Mutex::new(true),
            }
        }
    }

    #[async_trait]
    impl ClientIface for TestClientIface {
        async fn trigger_scan(&self) -> Result<ScanEnd, Error> {
            self.calls.lock().push(ClientIfaceCall::TriggerScan);
            let scan_end_receiver = self.scan_end_receiver.lock().take();
            match scan_end_receiver {
                Some(receiver) => receiver.await.expect("scan_end_signal failed"),
                None => Ok(ScanEnd::Complete),
            }
        }
        async fn abort_scan(&self) -> Result<(), Error> {
            self.calls.lock().push(ClientIfaceCall::AbortScan);
            Ok(())
        }
        fn get_last_scan_results(&self) -> Vec<fidl_sme::ScanResult> {
            self.calls.lock().push(ClientIfaceCall::GetLastScanResults);
            vec![fake_scan_result()]
        }
        async fn connect_to_network(
            &self,
            ssid: &[u8],
            passphrase: Option<Vec<u8>>,
            bssid: Option<Bssid>,
        ) -> Result<ConnectResult, Error> {
            self.calls.lock().push(ClientIfaceCall::ConnectToNetwork {
                ssid: ssid.to_vec(),
                passphrase: passphrase.clone(),
                bssid,
            });
            if *self.connect_success.lock() {
                let (proxy, server) =
                    fidl::endpoints::create_proxy::<fidl_sme::ConnectTransactionMarker>()
                        .expect("Failed to create fidl endpoints");
                let (_, handle) = server
                    .into_stream_and_control_handle()
                    .expect("Failed to get connect transaction control handle");
                *self.transaction_handle.lock() = Some(handle);
                Ok(ConnectResult::Success(ConnectSuccess {
                    bss: Box::new(random_bss_description!(
                        ssid: Ssid::try_from(ssid).unwrap(),
                        bssid: bssid.map(|b| b.to_array()).unwrap_or([42, 42, 42, 42, 42, 42]),
                    )),
                    transaction_stream: proxy.take_event_stream(),
                }))
            } else {
                Ok(ConnectResult::Fail(ConnectFail {
                    bss: Box::new(random_bss_description!(
                        ssid: Ssid::try_from(ssid).unwrap(),
                        bssid: bssid.map(|b| b.to_array()).unwrap_or([42, 42, 42, 42, 42, 42]),
                    )),
                    status_code: fidl_ieee80211::StatusCode::RefusedReasonUnspecified,
                    timed_out: false,
                }))
            }
        }
        async fn disconnect(&self) -> Result<(), Error> {
            self.calls.lock().push(ClientIfaceCall::Disconnect);
            Ok(())
        }

        fn get_connected_network_rssi(&self) -> Option<i8> {
            self.calls.lock().push(ClientIfaceCall::GetConnectedNetworkRssi);
            Some(-30)
        }

        fn on_disconnect(&self, info: &fidl_sme::DisconnectSource) {
            self.calls.lock().push(ClientIfaceCall::OnDisconnect { info: info.clone() });
        }

        fn on_signal_report(&self, ind: fidl_internal::SignalReportIndication) {
            self.calls.lock().push(ClientIfaceCall::OnSignalReport { ind });
        }
    }

    // Iface IDs are not currently read out of this struct anywhere, but keep them for future tests.
    #[allow(dead_code)]
    #[derive(Debug, Clone)]
    pub enum IfaceManagerCall {
        ListPhys,
        ListIfaces,
        QueryIface(u16),
        CreateClientIface(u16),
        GetClientIface(u16),
        DestroyIface(u16),
    }

    pub struct TestIfaceManager {
        pub client_iface: Mutex<Option<Arc<TestClientIface>>>,
        pub calls: Arc<Mutex<Vec<IfaceManagerCall>>>,
    }

    impl TestIfaceManager {
        pub fn new() -> Self {
            Self { client_iface: Mutex::new(None), calls: Arc::new(Mutex::new(vec![])) }
        }

        pub fn new_with_client() -> Self {
            Self {
                client_iface: Mutex::new(Some(Arc::new(TestClientIface::new()))),
                calls: Arc::new(Mutex::new(vec![])),
            }
        }

        pub fn new_with_client_and_scan_end_sender(
        ) -> (Self, oneshot::Sender<Result<ScanEnd, Error>>) {
            let (sender, receiver) = oneshot::channel();
            (
                Self {
                    client_iface: Mutex::new(Some(Arc::new(TestClientIface {
                        scan_end_receiver: Mutex::new(Some(receiver)),
                        ..TestClientIface::new()
                    }))),
                    calls: Arc::new(Mutex::new(vec![])),
                },
                sender,
            )
        }

        pub fn get_client_iface(&self) -> Arc<TestClientIface> {
            Arc::clone(self.client_iface.lock().as_ref().expect("No client iface found"))
        }

        pub fn get_iface_call_history(&self) -> Arc<Mutex<Vec<ClientIfaceCall>>> {
            let iface = self.client_iface.lock();
            let iface_ref = iface.as_ref().expect("client iface should exist");
            Arc::clone(&iface_ref.calls)
        }
    }

    #[async_trait]
    impl IfaceManager for TestIfaceManager {
        type Client = TestClientIface;

        async fn list_phys(&self) -> Result<Vec<u16>, Error> {
            self.calls.lock().push(IfaceManagerCall::ListPhys);
            Ok(vec![1])
        }

        fn list_ifaces(&self) -> Vec<u16> {
            self.calls.lock().push(IfaceManagerCall::ListIfaces);
            if self.client_iface.lock().is_some() {
                vec![FAKE_IFACE_RESPONSE.id]
            } else {
                vec![]
            }
        }

        async fn get_country(&self, _phy_id: u16) -> Result<[u8; 2], Error> {
            Ok([b'W', b'W'])
        }

        async fn query_iface(
            &self,
            iface_id: u16,
        ) -> Result<fidl_device_service::QueryIfaceResponse, Error> {
            self.calls.lock().push(IfaceManagerCall::QueryIface(iface_id));
            if self.client_iface.lock().is_some() && iface_id == FAKE_IFACE_RESPONSE.id {
                Ok(FAKE_IFACE_RESPONSE)
            } else {
                Err(format_err!("Unexpected query for iface id {}", iface_id))
            }
        }

        async fn create_client_iface(&self, phy_id: u16) -> Result<u16, Error> {
            self.calls.lock().push(IfaceManagerCall::CreateClientIface(phy_id));
            assert!(self.client_iface.lock().is_none());
            let _ = self.client_iface.lock().replace(Arc::new(TestClientIface {
                scan_end_receiver: Mutex::new(None),
                ..TestClientIface::new()
            }));
            Ok(FAKE_IFACE_RESPONSE.id)
        }

        async fn get_client_iface(&self, iface_id: u16) -> Result<Arc<TestClientIface>, Error> {
            self.calls.lock().push(IfaceManagerCall::GetClientIface(iface_id));
            if iface_id == FAKE_IFACE_RESPONSE.id {
                match self.client_iface.lock().as_ref() {
                    Some(iface) => Ok(Arc::clone(iface)),
                    None => Err(format_err!("Unexpected get_client_iface when no client exists")),
                }
            } else {
                Err(format_err!("Unexpected get_client_iface for missing iface id {}", iface_id))
            }
        }

        async fn destroy_iface(&self, iface_id: u16) -> Result<(), Error> {
            self.calls.lock().push(IfaceManagerCall::DestroyIface(iface_id));
            *self.client_iface.lock() = None;
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_utils::FAKE_IFACE_RESPONSE;
    use super::*;
    use fidl::endpoints::create_proxy_and_stream;
    use futures::task::Poll;
    use futures::StreamExt;
    use ieee80211::{MacAddrBytes, Ssid};
    use test_case::test_case;
    use wlan_common::channel::{Cbw, Channel};
    use wlan_common::test_utils::fake_stas::FakeProtectionCfg;
    use wlan_common::{assert_variant, fake_fidl_bss_description};
    use {
        fidl_fuchsia_wlan_common_security as fidl_security,
        fidl_fuchsia_wlan_internal as fidl_internal, fuchsia_async as fasync,
    };

    fn setup_test() -> (
        fasync::TestExecutor,
        fidl_device_service::DeviceMonitorRequestStream,
        DeviceMonitorIfaceManager,
    ) {
        let exec = fasync::TestExecutor::new_with_fake_time();
        exec.set_fake_time(fasync::MonotonicInstant::from_nanos(0));

        let (monitor_svc, monitor_stream) =
            create_proxy_and_stream::<fidl_device_service::DeviceMonitorMarker>()
                .expect("Failed to create device monitor service");
        (
            exec,
            monitor_stream,
            DeviceMonitorIfaceManager { monitor_svc, ifaces: Mutex::new(HashMap::new()) },
        )
    }

    #[test]
    fn test_query_interface() {
        let (mut exec, mut monitor_stream, manager) = setup_test();
        let mut fut = manager.query_iface(FAKE_IFACE_RESPONSE.id);

        // We should query device monitor for info on the iface.
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);
        let (iface_id, responder) = assert_variant!(
                 exec.run_until_stalled(&mut monitor_stream.select_next_some()),
            Poll::Ready(Ok(fidl_device_service::DeviceMonitorRequest::QueryIface { iface_id, responder })) => (iface_id, responder));
        assert_eq!(iface_id, FAKE_IFACE_RESPONSE.id);
        responder.send(Ok(&FAKE_IFACE_RESPONSE)).expect("Failed to respond to QueryIfaceResponse");

        let result =
            assert_variant!(exec.run_until_stalled(&mut fut), Poll::Ready(Ok(info)) => info);
        assert_eq!(result, FAKE_IFACE_RESPONSE);
    }

    #[test]
    fn test_get_country() {
        let (mut exec, mut monitor_stream, manager) = setup_test();
        let mut fut = manager.get_country(123);

        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);
        let (phy_id, responder) = assert_variant!(
                 exec.run_until_stalled(&mut monitor_stream.select_next_some()),
            Poll::Ready(Ok(fidl_device_service::DeviceMonitorRequest::GetCountry { phy_id, responder })) => (phy_id, responder));
        assert_eq!(phy_id, 123);
        responder
            .send(Ok(&fidl_device_service::GetCountryResponse { alpha2: [b'A', b'B'] }))
            .expect("Failed to respond to GetCountry");

        let country =
            assert_variant!(exec.run_until_stalled(&mut fut), Poll::Ready(Ok(info)) => info);
        assert_eq!(country, [b'A', b'B']);
    }

    #[test]
    fn test_create_and_serve_client_iface() {
        let (mut exec, mut monitor_stream, manager) = setup_test();
        let mut fut = manager.create_client_iface(0);

        // No interfaces to begin.
        assert!(manager.list_ifaces().is_empty());

        // Indicate that there are no existing ifaces.
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);
        let responder = assert_variant!(
            exec.run_until_stalled(&mut monitor_stream.select_next_some()),
            Poll::Ready(Ok(fidl_device_service::DeviceMonitorRequest::ListIfaces { responder })) => responder);
        responder.send(&[]).expect("Failed to respond to ListIfaces");

        // Create a new iface.
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);
        let responder = assert_variant!(
            exec.run_until_stalled(&mut monitor_stream.select_next_some()),
            Poll::Ready(Ok(fidl_device_service::DeviceMonitorRequest::CreateIface { responder, .. })) => responder);
        responder
            .send(
                0,
                Some(&fidl_device_service::CreateIfaceResponse {
                    iface_id: FAKE_IFACE_RESPONSE.id,
                }),
            )
            .expect("Failed to send CreateIface response");

        // Establish a connection to the new iface.
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);
        let responder = assert_variant!(
            exec.run_until_stalled(&mut monitor_stream.select_next_some()),
            Poll::Ready(Ok(fidl_device_service::DeviceMonitorRequest::GetClientSme { responder, .. })) => responder);
        responder.send(Ok(())).expect("Failed to send GetClientSme response");

        // Creation complete!
        let request_id =
            assert_variant!(exec.run_until_stalled(&mut fut), Poll::Ready(Ok(id)) => id);
        assert_eq!(request_id, FAKE_IFACE_RESPONSE.id);

        // The new iface shows up in ListInterfaces.
        assert_eq!(manager.list_ifaces(), vec![FAKE_IFACE_RESPONSE.id]);

        // The new iface is ready for use.
        assert_variant!(
            exec.run_until_stalled(&mut manager.get_client_iface(FAKE_IFACE_RESPONSE.id)),
            Poll::Ready(Ok(_))
        );
    }

    #[test]
    fn test_create_iface_fails() {
        let (mut exec, mut monitor_stream, manager) = setup_test();
        let mut fut = manager.create_client_iface(0);

        // Indicate that there are no existing ifaces.
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);
        let responder = assert_variant!(
            exec.run_until_stalled(&mut monitor_stream.select_next_some()),
            Poll::Ready(Ok(fidl_device_service::DeviceMonitorRequest::ListIfaces { responder })) => responder);
        responder.send(&[]).expect("Failed to respond to ListIfaces");

        // Return an error for CreateIface.
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);
        let responder = assert_variant!(
            exec.run_until_stalled(&mut monitor_stream.select_next_some()),
            Poll::Ready(Ok(fidl_device_service::DeviceMonitorRequest::CreateIface { responder, .. })) => responder);
        responder.send(1, None).expect("Failed to send CreateIface response");

        assert_variant!(
            exec.run_until_stalled(&mut manager.get_client_iface(FAKE_IFACE_RESPONSE.id)),
            Poll::Ready(Err(_))
        );
    }

    // TODO(b/298030838): Delete test when wlanix is the sole config path.
    #[test]
    fn test_create_iface_with_unmanaged() {
        let (mut exec, mut monitor_stream, manager) = setup_test();
        let mut fut = manager.create_client_iface(0);

        // No interfaces to begin.
        assert!(manager.list_ifaces().is_empty());

        // Indicate that there is a fake iface.
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);
        let responder = assert_variant!(
            exec.run_until_stalled(&mut monitor_stream.select_next_some()),
            Poll::Ready(Ok(fidl_device_service::DeviceMonitorRequest::ListIfaces { responder })) => responder);
        responder.send(&[FAKE_IFACE_RESPONSE.id]).expect("Failed to respond to ListIfaces");

        // Respond with iface info.
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);
        let (iface_id, responder) = assert_variant!(
            exec.run_until_stalled(&mut monitor_stream.select_next_some()),
            Poll::Ready(Ok(fidl_device_service::DeviceMonitorRequest::QueryIface { iface_id, responder })) => (iface_id, responder));
        assert_eq!(iface_id, FAKE_IFACE_RESPONSE.id);
        responder.send(Ok(&FAKE_IFACE_RESPONSE)).expect("Failed to respond to QueryIface");

        // Establish a connection to the new iface.
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);
        let responder = assert_variant!(
            exec.run_until_stalled(&mut monitor_stream.select_next_some()),
            Poll::Ready(Ok(fidl_device_service::DeviceMonitorRequest::GetClientSme { responder, .. })) => responder);
        responder.send(Ok(())).expect("Failed to send GetClientSme response");

        // We finish up and have a new iface.
        let id = assert_variant!(exec.run_until_stalled(&mut fut), Poll::Ready(Ok(id)) => id);
        assert_eq!(id, FAKE_IFACE_RESPONSE.id);
        assert_eq!(&manager.list_ifaces()[..], [id]);
    }

    #[test]
    fn test_destroy_iface() {
        let (mut exec, mut monitor_stream, manager) = setup_test();

        let (proxy, _server) =
            create_proxy::<fidl_sme::ClientSmeMarker>().expect("Failed to create proxy");
        manager.ifaces.lock().insert(1, Arc::new(SmeClientIface::new(proxy)));

        let mut fut = manager.destroy_iface(1);

        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);
        let responder = assert_variant!(
            exec.run_until_stalled(&mut monitor_stream.select_next_some()),
            Poll::Ready(Ok(fidl_device_service::DeviceMonitorRequest::DestroyIface { responder, .. })) => responder);
        responder.send(0).expect("Failed to send DestroyIface response");
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Ready(Ok(())));

        assert!(manager.ifaces.lock().is_empty());
    }

    // TODO(b/298030838): Delete test when wlanix is the sole config path.
    #[test]
    fn test_destroy_iface_not_wlanix() {
        let (mut exec, mut monitor_stream, manager) = setup_test();

        let (proxy, _server) =
            create_proxy::<fidl_sme::ClientSmeMarker>().expect("Failed to create proxy");
        let mut iface = SmeClientIface::new(proxy);
        iface.wlanix_provisioned = false;
        manager.ifaces.lock().insert(1, Arc::new(iface));

        let mut fut = manager.destroy_iface(1);

        // No destroy request is sent.
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Ready(Ok(())));
        assert_variant!(
            exec.run_until_stalled(&mut monitor_stream.select_next_some()),
            Poll::Pending
        );

        assert!(manager.ifaces.lock().is_empty());
    }

    #[test]
    fn test_get_client_iface_fails_no_such_iface() {
        let (mut exec, _monitor_stream, manager) = setup_test();
        let mut fut = manager.get_client_iface(1);

        // No ifaces exist, so this should always error.
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Ready(Err(_e)));
    }

    #[test]
    fn test_destroy_iface_no_such_iface() {
        let (mut exec, _monitor_stream, manager) = setup_test();
        let mut fut = manager.destroy_iface(1);

        // No ifaces exist, so this should always return immediately.
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Ready(Ok(())));
    }

    #[test]
    fn test_trigger_scan() {
        let (mut exec, _monitor_stream, manager) = setup_test();
        let (sme_proxy, mut sme_stream) = create_proxy_and_stream::<fidl_sme::ClientSmeMarker>()
            .expect("Failed to create device monitor service");
        manager.ifaces.lock().insert(1, Arc::new(SmeClientIface::new(sme_proxy)));
        let mut client_fut = manager.get_client_iface(1);
        let iface = assert_variant!(exec.run_until_stalled(&mut client_fut), Poll::Ready(Ok(iface)) => iface);
        assert!(iface.get_last_scan_results().is_empty());
        let mut scan_fut = iface.trigger_scan();
        assert_variant!(exec.run_until_stalled(&mut scan_fut), Poll::Pending);
        let (_req, responder) = assert_variant!(
            exec.run_until_stalled(&mut sme_stream.next()),
            Poll::Ready(Some(Ok(fidl_sme::ClientSmeRequest::Scan { req, responder }))) => (req, responder));
        let result = wlan_common::scan::write_vmo(vec![test_utils::fake_scan_result()])
            .expect("Failed to write scan VMO");
        responder.send(Ok(result)).expect("Failed to send result");
        assert_variant!(exec.run_until_stalled(&mut scan_fut), Poll::Ready(Ok(ScanEnd::Complete)));
        assert_eq!(iface.get_last_scan_results().len(), 1);
    }

    #[test]
    fn test_abort_scan() {
        let (mut exec, _monitor_stream, manager) = setup_test();
        let (sme_proxy, mut sme_stream) = create_proxy_and_stream::<fidl_sme::ClientSmeMarker>()
            .expect("Failed to create device monitor service");
        manager.ifaces.lock().insert(1, Arc::new(SmeClientIface::new(sme_proxy)));
        let mut client_fut = manager.get_client_iface(1);
        let iface = assert_variant!(exec.run_until_stalled(&mut client_fut), Poll::Ready(Ok(iface)) => iface);
        assert!(iface.get_last_scan_results().is_empty());
        let mut scan_fut = iface.trigger_scan();
        assert_variant!(exec.run_until_stalled(&mut scan_fut), Poll::Pending);
        let (_req, _responder) = assert_variant!(
            exec.run_until_stalled(&mut sme_stream.next()),
            Poll::Ready(Some(Ok(fidl_sme::ClientSmeRequest::Scan { req, responder }))) => (req, responder));

        // trigger_scan returns after we abort the scan, even though we have no results from SME.
        assert_variant!(exec.run_until_stalled(&mut iface.abort_scan()), Poll::Ready(Ok(())));
        assert_variant!(exec.run_until_stalled(&mut scan_fut), Poll::Ready(Ok(ScanEnd::Cancelled)));
    }

    #[test_case(
        FakeProtectionCfg::Open,
        vec![fidl_security::Protocol::Open],
        None,
        false,
        fidl_security::Authentication {
            protocol: fidl_security::Protocol::Open,
            credentials: None
        };
        "open_any_bssid"
    )]
    #[test_case(
        FakeProtectionCfg::Wpa2,
        vec![fidl_security::Protocol::Wpa2Personal],
        Some(b"password".to_vec()),
        false,
        fidl_security::Authentication {
            protocol: fidl_security::Protocol::Wpa2Personal,
            credentials: Some(Box::new(fidl_security::Credentials::Wpa(
                fidl_security::WpaCredentials::Passphrase(b"password".to_vec())
            )))
        };
        "wpa2_any_bssid"
    )]
    #[test_case(
        FakeProtectionCfg::Open,
        vec![fidl_security::Protocol::Open],
        None,
        false,
        fidl_security::Authentication {
            protocol: fidl_security::Protocol::Open,
            credentials: None
        };
        "bssid_specified"
    )]
    #[fuchsia::test(add_test_attr = false)]
    fn test_connect_to_network(
        fake_protection_cfg: FakeProtectionCfg,
        mutual_security_protocols: Vec<fidl_security::Protocol>,
        passphrase: Option<Vec<u8>>,
        bssid_specified: bool,
        expected_authentication: fidl_security::Authentication,
    ) {
        let (mut exec, _monitor_stream, manager) = setup_test();
        let (sme_proxy, mut sme_stream) = create_proxy_and_stream::<fidl_sme::ClientSmeMarker>()
            .expect("Failed to create device monitor service");
        manager.ifaces.lock().insert(1, Arc::new(SmeClientIface::new(sme_proxy)));
        let mut client_fut = manager.get_client_iface(1);
        let iface = assert_variant!(exec.run_until_stalled(&mut client_fut), Poll::Ready(Ok(iface)) => iface);

        let bss_description = fake_fidl_bss_description!(protection => fake_protection_cfg,
            ssid: Ssid::try_from("foo").unwrap(),
            bssid: [1, 2, 3, 4, 5, 6],
        );
        *iface.last_scan_results.lock() = vec![fidl_sme::ScanResult {
            bss_description: bss_description.clone(),
            compatibility: Some(Box::new(fidl_sme::Compatibility { mutual_security_protocols })),
            timestamp_nanos: 1,
        }];

        let bssid = if bssid_specified { Some(Bssid::from([1, 2, 3, 4, 5, 6])) } else { None };
        let mut connect_fut = iface.connect_to_network(&[b'f', b'o', b'o'], passphrase, bssid);
        assert_variant!(exec.run_until_stalled(&mut connect_fut), Poll::Pending);
        let (req, connect_txn) = assert_variant!(
            exec.run_until_stalled(&mut sme_stream.next()),
            Poll::Ready(Some(Ok(fidl_sme::ClientSmeRequest::Connect { req, txn: Some(txn), .. }))) => (req, txn));
        assert_eq!(req.bss_description, bss_description);
        assert_eq!(req.authentication, expected_authentication);

        let connect_txn_handle = connect_txn.into_stream_and_control_handle().unwrap().1;
        let result = connect_txn_handle.send_on_connect_result(&fidl_sme::ConnectResult {
            code: fidl_ieee80211::StatusCode::Success,
            is_credential_rejected: false,
            is_reconnect: false,
        });
        assert_variant!(result, Ok(()));

        let connect_result =
            assert_variant!(exec.run_until_stalled(&mut connect_fut), Poll::Ready(r) => r);
        let connected_result = assert_variant!(connect_result, Ok(ConnectResult::Success(r)) => r);
        assert_eq!(connected_result.bss.ssid, Ssid::try_from("foo").unwrap());
        assert_eq!(connected_result.bss.bssid, Bssid::from([1, 2, 3, 4, 5, 6]));
    }

    #[test_case(
        false,
        FakeProtectionCfg::Open,
        vec![fidl_security::Protocol::Open],
        None,
        None;
        "network_not_found"
    )]
    #[test_case(
        true,
        FakeProtectionCfg::Open,
        vec![fidl_security::Protocol::Open],
        Some(b"password".to_vec()),
        None;
        "open_with_password"
    )]
    #[test_case(
        true,
        FakeProtectionCfg::Wpa2,
        vec![fidl_security::Protocol::Wpa2Personal],
        None,
        None;
        "wpa2_without_password"
    )]
    #[test_case(
        true,
        FakeProtectionCfg::Wpa2,
        vec![fidl_security::Protocol::Open],
        None,
        Some([24, 51, 32, 52, 41, 32].into());
        "bssid_not_found"
    )]
    #[fuchsia::test(add_test_attr = false)]
    fn test_connect_rejected(
        has_network: bool,
        fake_protection_cfg: FakeProtectionCfg,
        mutual_security_protocols: Vec<fidl_security::Protocol>,
        passphrase: Option<Vec<u8>>,
        bssid: Option<Bssid>,
    ) {
        let (mut exec, _monitor_stream, manager) = setup_test();
        let (sme_proxy, mut _sme_stream) = create_proxy_and_stream::<fidl_sme::ClientSmeMarker>()
            .expect("Failed to create device monitor service");
        manager.ifaces.lock().insert(1, Arc::new(SmeClientIface::new(sme_proxy)));
        let mut client_fut = manager.get_client_iface(1);
        let iface = assert_variant!(exec.run_until_stalled(&mut client_fut), Poll::Ready(Ok(iface)) => iface);

        if has_network {
            let bss_description = fake_fidl_bss_description!(protection => fake_protection_cfg,
                ssid: Ssid::try_from("foo").unwrap(),
                bssid: [1, 2, 3, 4, 5, 6],
            );
            *iface.last_scan_results.lock() = vec![fidl_sme::ScanResult {
                bss_description: bss_description.clone(),
                compatibility: Some(Box::new(fidl_sme::Compatibility {
                    mutual_security_protocols,
                })),
                timestamp_nanos: 1,
            }];
        }

        let mut connect_fut = iface.connect_to_network(&[b'f', b'o', b'o'], passphrase, bssid);
        assert_variant!(exec.run_until_stalled(&mut connect_fut), Poll::Ready(Err(_e)));
    }

    #[test]
    fn test_connect_fails_at_sme() {
        let (mut exec, _monitor_stream, manager) = setup_test();
        let (sme_proxy, mut sme_stream) = create_proxy_and_stream::<fidl_sme::ClientSmeMarker>()
            .expect("Failed to create device monitor service");
        manager.ifaces.lock().insert(1, Arc::new(SmeClientIface::new(sme_proxy)));
        let mut client_fut = manager.get_client_iface(1);
        let iface = assert_variant!(exec.run_until_stalled(&mut client_fut), Poll::Ready(Ok(iface)) => iface);

        let bss_description = fake_fidl_bss_description!(Open,
            ssid: Ssid::try_from("foo").unwrap(),
            bssid: [1, 2, 3, 4, 5, 6],
        );
        *iface.last_scan_results.lock() = vec![fidl_sme::ScanResult {
            bss_description: bss_description.clone(),
            compatibility: Some(Box::new(fidl_sme::Compatibility {
                mutual_security_protocols: vec![fidl_security::Protocol::Open],
            })),
            timestamp_nanos: 1,
        }];

        let mut connect_fut = iface.connect_to_network(&[b'f', b'o', b'o'], None, None);
        assert_variant!(exec.run_until_stalled(&mut connect_fut), Poll::Pending);
        let (req, connect_txn) = assert_variant!(
            exec.run_until_stalled(&mut sme_stream.next()),
            Poll::Ready(Some(Ok(fidl_sme::ClientSmeRequest::Connect { req, txn: Some(txn), .. }))) => (req, txn));
        assert_eq!(req.bss_description, bss_description);
        assert_eq!(
            req.authentication,
            fidl_security::Authentication {
                protocol: fidl_security::Protocol::Open,
                credentials: None,
            }
        );

        let connect_txn_handle = connect_txn.into_stream_and_control_handle().unwrap().1;
        let result = connect_txn_handle.send_on_connect_result(&fidl_sme::ConnectResult {
            code: fidl_ieee80211::StatusCode::RefusedExternalReason,
            is_credential_rejected: false,
            is_reconnect: false,
        });
        assert_variant!(result, Ok(()));

        let connect_result =
            assert_variant!(exec.run_until_stalled(&mut connect_fut), Poll::Ready(Ok(r)) => r);
        let failure = assert_variant!(connect_result, ConnectResult::Fail(failure) => failure);
        assert_eq!(failure.status_code, fidl_ieee80211::StatusCode::RefusedExternalReason);
        assert!(!failure.timed_out);
    }

    #[test]
    fn test_connect_fails_with_timeout() {
        let (mut exec, _monitor_stream, manager) = setup_test();
        let (sme_proxy, mut sme_stream) = create_proxy_and_stream::<fidl_sme::ClientSmeMarker>()
            .expect("Failed to create device monitor service");
        manager.ifaces.lock().insert(1, Arc::new(SmeClientIface::new(sme_proxy)));
        let mut client_fut = manager.get_client_iface(1);
        let iface = assert_variant!(exec.run_until_stalled(&mut client_fut), Poll::Ready(Ok(iface)) => iface);

        let bss_description = fake_fidl_bss_description!(Open,
            ssid: Ssid::try_from("foo").unwrap(),
            bssid: [1, 2, 3, 4, 5, 6],
        );
        *iface.last_scan_results.lock() = vec![fidl_sme::ScanResult {
            bss_description: bss_description.clone(),
            compatibility: Some(Box::new(fidl_sme::Compatibility {
                mutual_security_protocols: vec![fidl_security::Protocol::Open],
            })),
            timestamp_nanos: 1,
        }];

        let mut connect_fut = iface.connect_to_network(&[b'f', b'o', b'o'], None, None);
        assert_variant!(exec.run_until_stalled(&mut connect_fut), Poll::Pending);
        let (_req, _connect_txn) = assert_variant!(
            exec.run_until_stalled(&mut sme_stream.next()),
            Poll::Ready(Some(Ok(fidl_sme::ClientSmeRequest::Connect { req, txn: Some(txn), .. }))) => (req, txn));
        exec.set_fake_time(fasync::MonotonicInstant::from_nanos(40_000_000_000));

        let connect_result =
            assert_variant!(exec.run_until_stalled(&mut connect_fut), Poll::Ready(Ok(r)) => r);
        let failure = assert_variant!(connect_result, ConnectResult::Fail(failure) => failure);
        assert!(failure.timed_out);
    }

    #[test_case(
        vec![
            fake_fidl_bss_description!(Open,
                ssid: Ssid::try_from("foo").unwrap(),
                bssid: [1, 2, 3, 4, 5, 6],
                channel: Channel::new(1, Cbw::Cbw20),
                rssi_dbm: -40,
            ),
            fake_fidl_bss_description!(Open,
                ssid: Ssid::try_from("foo").unwrap(),
                bssid: [2, 3, 4, 5, 6, 7],
                channel: Channel::new(1, Cbw::Cbw20),
                rssi_dbm: -30,
            ),
            fake_fidl_bss_description!(Open,
                ssid: Ssid::try_from("foo").unwrap(),
                bssid: [3, 4, 5, 6, 7, 8],
                channel: Channel::new(1, Cbw::Cbw20),
                rssi_dbm: -50,
            ),
        ],
        None,
        Bssid::from([2, 3, 4, 5, 6, 7]);
        "no_penalty"
    )]
    #[test_case(
        vec![
            fake_fidl_bss_description!(Open,
                ssid: Ssid::try_from("foo").unwrap(),
                bssid: [1, 2, 3, 4, 5, 6],
                channel: Channel::new(1, Cbw::Cbw20),
                rssi_dbm: -40,
            ),
            fake_fidl_bss_description!(Open,
                ssid: Ssid::try_from("foo").unwrap(),
                bssid: [2, 3, 4, 5, 6, 7],
                channel: Channel::new(1, Cbw::Cbw20),
                rssi_dbm: -30,
            ),
            fake_fidl_bss_description!(Open,
                ssid: Ssid::try_from("foo").unwrap(),
                bssid: [3, 4, 5, 6, 7, 8],
                channel: Channel::new(1, Cbw::Cbw20),
                rssi_dbm: -50,
            ),
        ],
        Some((
            fake_fidl_bss_description!(Open,
                ssid: Ssid::try_from("foo").unwrap(),
                bssid: [2, 3, 4, 5, 6, 7],
                channel: Channel::new(1, Cbw::Cbw20),
                rssi_dbm: -30,
            ),
            fidl_sme::ConnectResult {
                code: fidl_ieee80211::StatusCode::RefusedExternalReason,
                is_credential_rejected: true,
                is_reconnect: false,
            }
        )),
        Bssid::from([1, 2, 3, 4, 5, 6]);
        "recent_connect_failure"
    )]
    #[fuchsia::test(add_test_attr = false)]
    fn test_connect_to_network_bss_selection(
        scan_bss_descriptions: Vec<fidl_common::BssDescription>,
        recent_connect_failure: Option<(fidl_common::BssDescription, fidl_sme::ConnectResult)>,
        expected_bssid: Bssid,
    ) {
        let (mut exec, _monitor_stream, manager) = setup_test();
        let (sme_proxy, mut sme_stream) = create_proxy_and_stream::<fidl_sme::ClientSmeMarker>()
            .expect("Failed to create device monitor service");
        manager.ifaces.lock().insert(1, Arc::new(SmeClientIface::new(sme_proxy)));
        let mut client_fut = manager.get_client_iface(1);
        let iface = assert_variant!(exec.run_until_stalled(&mut client_fut), Poll::Ready(Ok(iface)) => iface);

        if let Some((bss_description, connect_failure)) = recent_connect_failure {
            // Set up a connect failure so that later in the test, there'd be a score penalty
            // for the BSS described by `bss_description`
            *iface.last_scan_results.lock() = vec![fidl_sme::ScanResult {
                bss_description: bss_description,
                compatibility: Some(Box::new(fidl_sme::Compatibility {
                    mutual_security_protocols: vec![fidl_security::Protocol::Open],
                })),
                timestamp_nanos: 1,
            }];

            let mut connect_fut = iface.connect_to_network(&[b'f', b'o', b'o'], None, None);
            assert_variant!(exec.run_until_stalled(&mut connect_fut), Poll::Pending);
            let (_req, connect_txn) = assert_variant!(
                exec.run_until_stalled(&mut sme_stream.next()),
                Poll::Ready(Some(Ok(fidl_sme::ClientSmeRequest::Connect { req, txn: Some(txn), .. }))) => (req, txn));
            let connect_txn_handle = connect_txn.into_stream_and_control_handle().unwrap().1;
            let _result = connect_txn_handle.send_on_connect_result(&connect_failure);
            assert_variant!(exec.run_until_stalled(&mut connect_fut), Poll::Ready(Ok(_r)));
        }

        *iface.last_scan_results.lock() = scan_bss_descriptions
            .into_iter()
            .map(|bss_description| fidl_sme::ScanResult {
                bss_description,
                compatibility: Some(Box::new(fidl_sme::Compatibility {
                    mutual_security_protocols: vec![fidl_security::Protocol::Open],
                })),
                timestamp_nanos: 1,
            })
            .collect::<Vec<_>>();

        let mut connect_fut = iface.connect_to_network(&[b'f', b'o', b'o'], None, None);
        assert_variant!(exec.run_until_stalled(&mut connect_fut), Poll::Pending);
        let (req, _connect_txn) = assert_variant!(
            exec.run_until_stalled(&mut sme_stream.next()),
            Poll::Ready(Some(Ok(fidl_sme::ClientSmeRequest::Connect { req, txn: Some(txn), .. }))) => (req, txn));
        assert_eq!(req.bss_description.bssid, expected_bssid.to_array());
    }

    #[test]
    fn test_disconnect() {
        let (mut exec, _monitor_stream, manager) = setup_test();
        let (sme_proxy, mut sme_stream) = create_proxy_and_stream::<fidl_sme::ClientSmeMarker>()
            .expect("Failed to create device monitor service");
        manager.ifaces.lock().insert(1, Arc::new(SmeClientIface::new(sme_proxy)));
        let mut client_fut = manager.get_client_iface(1);
        let iface = assert_variant!(exec.run_until_stalled(&mut client_fut), Poll::Ready(Ok(iface)) => iface);

        let mut disconnect_fut = iface.disconnect();
        assert_variant!(exec.run_until_stalled(&mut disconnect_fut), Poll::Pending);
        let (disconnect_reason, disconnect_responder) = assert_variant!(
            exec.run_until_stalled(&mut sme_stream.next()),
            Poll::Ready(Some(Ok(fidl_sme::ClientSmeRequest::Disconnect { reason, responder }))) => (reason, responder));
        assert_eq!(disconnect_reason, fidl_sme::UserDisconnectReason::Unknown);

        assert_variant!(disconnect_responder.send(), Ok(()));
        assert_variant!(exec.run_until_stalled(&mut disconnect_fut), Poll::Ready(Ok(())));
    }

    #[test]
    fn test_on_disconnect() {
        let (mut exec, _monitor_stream, manager) = setup_test();
        let (sme_proxy, _sme_stream) = create_proxy_and_stream::<fidl_sme::ClientSmeMarker>()
            .expect("Failed to create device monitor service");
        manager.ifaces.lock().insert(1, Arc::new(SmeClientIface::new(sme_proxy)));
        let mut client_fut = manager.get_client_iface(1);
        let iface = assert_variant!(exec.run_until_stalled(&mut client_fut), Poll::Ready(Ok(iface)) => iface);

        iface.on_signal_report(fidl_internal::SignalReportIndication { rssi_dbm: -40, snr_db: 20 });
        assert_variant!(iface.get_connected_network_rssi(), Some(-40));
        iface.on_disconnect(&fidl_sme::DisconnectSource::User(
            fidl_sme::UserDisconnectReason::Unknown,
        ));
        assert_variant!(iface.get_connected_network_rssi(), None);
    }

    #[test]
    fn test_on_signal_report() {
        let (mut exec, _monitor_stream, manager) = setup_test();
        let (sme_proxy, _sme_stream) = create_proxy_and_stream::<fidl_sme::ClientSmeMarker>()
            .expect("Failed to create device monitor service");
        manager.ifaces.lock().insert(1, Arc::new(SmeClientIface::new(sme_proxy)));
        let mut client_fut = manager.get_client_iface(1);
        let iface = assert_variant!(exec.run_until_stalled(&mut client_fut), Poll::Ready(Ok(iface)) => iface);

        assert_variant!(iface.get_connected_network_rssi(), None);
        iface.on_signal_report(fidl_internal::SignalReportIndication { rssi_dbm: -40, snr_db: 20 });
        assert_variant!(iface.get_connected_network_rssi(), Some(-40));
    }
}
