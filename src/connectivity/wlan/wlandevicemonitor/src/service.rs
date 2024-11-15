// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::device::{self, IfaceDevice, IfaceMap, NewIface, PhyDevice, PhyMap};
use crate::watcher_service;
use anyhow::{Context, Error};
use core::sync::atomic::AtomicUsize;
use fidl::endpoints::create_endpoints;
use fidl::HandleBased;
use fidl_fuchsia_wlan_device_service::{self as fidl_svc, DeviceMonitorRequest};
use fuchsia_inspect::Inspector;
use futures::channel::mpsc;
use futures::stream::FuturesUnordered;
use futures::{select, FutureExt, StreamExt, TryStreamExt};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tracing::{error, info, warn};
use {
    fidl_fuchsia_wlan_common as fidl_common, fidl_fuchsia_wlan_device as fidl_dev,
    fidl_fuchsia_wlan_sme as fidl_sme,
};

/// Thread-safe counter for spawned ifaces.
pub struct IfaceCounter(AtomicUsize);

impl IfaceCounter {
    pub fn new() -> Self {
        Self(AtomicUsize::new(0))
    }

    /// Provides the caller with a new unique id.
    pub fn next_iface_id(&self) -> usize {
        self.0.fetch_add(1, Ordering::SeqCst)
    }
}

pub(crate) async fn serve_monitor_requests(
    mut req_stream: fidl_svc::DeviceMonitorRequestStream,
    phys: Arc<PhyMap>,
    ifaces: Arc<IfaceMap>,
    watcher_service: watcher_service::WatcherService<PhyDevice, IfaceDevice>,
    new_iface_sink: mpsc::UnboundedSender<NewIface>,
    iface_counter: Arc<IfaceCounter>,
    mut ifaces_node: fuchsia_inspect::Node,
    cfg: wlandevicemonitor_config::Config,
) -> Result<(), Error> {
    while let Some(req) = req_stream.try_next().await.context("error running DeviceService")? {
        match req {
            DeviceMonitorRequest::ListPhys { responder } => responder.send(&list_phys(&phys)),
            DeviceMonitorRequest::ListIfaces { responder } => responder.send(&list_ifaces(&ifaces)),
            DeviceMonitorRequest::GetDevPath { phy_id, responder } => {
                responder.send(get_dev_path(&phys, phy_id).as_deref())
            }
            DeviceMonitorRequest::GetSupportedMacRoles { phy_id, responder } => responder
                .send(get_supported_mac_roles(&phys, phy_id).await.as_deref().map_err(|e| *e)),
            DeviceMonitorRequest::WatchDevices { watcher, control_handle: _ } => {
                watcher_service
                    .add_watcher(watcher)
                    .unwrap_or_else(|e| error!("error registering a device watcher: {}", e));
                Ok(())
            }
            DeviceMonitorRequest::GetCountry { phy_id, responder } => {
                responder.send(get_country(&phys, phy_id).await.as_ref().map_err(|s| s.into_raw()))
            }
            DeviceMonitorRequest::SetCountry { req, responder } => {
                let status = set_country(&phys, req).await;
                responder.send(status.into_raw())
            }
            DeviceMonitorRequest::ClearCountry { req, responder } => {
                let status = clear_country(&phys, req).await;
                responder.send(status.into_raw())
            }
            DeviceMonitorRequest::SetPowerSaveMode { req, responder } => {
                let status = set_power_save_mode(&phys, req).await;
                responder.send(status.into_raw())
            }
            DeviceMonitorRequest::GetPowerSaveMode { phy_id, responder } => responder
                .send(get_power_save_mode(&phys, phy_id).await.as_ref().map_err(|s| s.into_raw())),
            DeviceMonitorRequest::CreateIface { req, responder } => {
                match create_iface(
                    &new_iface_sink,
                    &phys,
                    req,
                    &iface_counter,
                    &cfg,
                    &mut ifaces_node,
                )
                .await
                {
                    Ok(new_iface) => {
                        info!("iface #{} started ({:?})", new_iface.id, new_iface.phy_ownership);
                        ifaces.insert(
                            new_iface.id,
                            IfaceDevice {
                                phy_ownership: new_iface.phy_ownership,
                                generic_sme: new_iface.generic_sme,
                                inspect_node: new_iface.inspect_node,
                                inspect_vmo: new_iface.inspect_vmo,
                            },
                        );

                        let resp = fidl_svc::CreateIfaceResponse { iface_id: new_iface.id };
                        responder.send(zx::sys::ZX_OK, Some(&resp))
                    }
                    Err(status) => responder.send(status.into_raw(), None),
                }
            }
            DeviceMonitorRequest::QueryIface { iface_id, responder } => {
                let result = query_iface(&ifaces, iface_id).await;
                responder.send(result.as_ref().map_err(|e| e.into_raw()))
            }
            DeviceMonitorRequest::DestroyIface { req, responder } => {
                let result =
                    destroy_iface(&phys, &ifaces, ifaces_node.clone_weak(), req.iface_id).await;
                let status = into_status_and_opt(result).0;
                responder.send(status.into_raw())
            }
            DeviceMonitorRequest::GetClientSme { iface_id, sme_server, responder } => {
                let result = get_client_sme(&ifaces, iface_id, sme_server).await;
                responder.send(result.map_err(|e| e.into_raw()))
            }
            DeviceMonitorRequest::GetApSme { iface_id, sme_server, responder } => {
                let result = get_ap_sme(&ifaces, iface_id, sme_server).await;
                responder.send(result.map_err(|e| e.into_raw()))
            }
            DeviceMonitorRequest::GetSmeTelemetry { iface_id, telemetry_server, responder } => {
                let result = get_sme_telemetry(&ifaces, iface_id, telemetry_server).await;
                responder.send(result.map_err(|e| e.into_raw()))
            }
            DeviceMonitorRequest::GetFeatureSupport {
                iface_id,
                feature_support_server,
                responder,
            } => {
                let result = get_feature_support(&ifaces, iface_id, feature_support_server).await;
                responder.send(result.map_err(|e| e.into_raw()))
            }
        }?;
    }

    Ok(())
}

pub(crate) async fn handle_new_iface_stream(
    phys: Arc<PhyMap>,
    ifaces: Arc<IfaceMap>,
    ifaces_node: fuchsia_inspect::Node,
    mut iface_stream: mpsc::UnboundedReceiver<NewIface>,
) -> Result<(), Error> {
    let mut futures_unordered = FuturesUnordered::new();
    loop {
        select! {
            _ = futures_unordered.select_next_some() => (),
            new_iface = iface_stream.next() => {
                if let Some(new_iface) = new_iface {
                    futures_unordered.push(
                        handle_single_new_iface(
                            phys.clone(),
                            ifaces.clone(),
                            ifaces_node.clone_weak(),
                            new_iface
                        )
                    );
                }
            }
        }
    }
}

async fn handle_single_new_iface(
    phys: Arc<PhyMap>,
    ifaces: Arc<IfaceMap>,
    ifaces_node: fuchsia_inspect::Node,
    new_iface: NewIface,
) {
    let mut event_stream = new_iface.generic_sme.take_event_stream().fuse();
    // GenericSme does not currently have any client-bound messages, so this is
    // exclusively to monitor for the channel epitaph and respond appropriately.
    // A clean SME shutdown should always result in an OK epitaph -- initiate
    // iface destruction on any other behavior to remove stale info.
    while !event_stream.is_done() {
        match event_stream.next().await {
            None => (),
            Some(Err(fidl::Error::ClientChannelClosed { status, .. })) => match status {
                zx::Status::OK => {
                    info!("Generic SME event stream closed cleanly.");
                    return;
                }
                e => {
                    error!("Generic SME event stream closed with error: {}", e);
                    break;
                }
            },
            Some(message) => {
                error!(
                    "Unexpected message from Generic SME event stream: {:?}. Aborting Generic SME.",
                    message
                );
                break;
            }
        }
    }
    match destroy_iface(&phys, &ifaces, ifaces_node, new_iface.id).await {
        Ok(()) => info!("Destroyed iface {}", new_iface.id),
        Err(e) if e == zx::Status::NOT_FOUND => {
            warn!("destroy_iface - iface {} not found; assume success", new_iface.id)
        }
        Err(e) => error!("Error while destroying iface {}: {}", new_iface.id, e),
    }
}

fn list_phys(phys: &PhyMap) -> Vec<u16> {
    phys.get_snapshot().iter().map(|(phy_id, _)| *phy_id).collect()
}

fn list_ifaces(ifaces: &IfaceMap) -> Vec<u16> {
    ifaces.get_snapshot().iter().map(|(iface_id, _)| *iface_id).collect()
}

fn get_dev_path(phys: &PhyMap, phy_id: u16) -> Option<String> {
    let phy = phys.get(&phy_id)?;
    Some(phy.device_path.clone())
}

async fn get_country(
    phys: &PhyMap,
    phy_id: u16,
) -> Result<fidl_svc::GetCountryResponse, zx::Status> {
    let phy = phys.get(&phy_id).ok_or(Err(zx::Status::NOT_FOUND))?;
    match phy.proxy.get_country().await {
        Ok(result) => match result {
            Ok(country_code) => Ok(fidl_svc::GetCountryResponse { alpha2: country_code.alpha2 }),
            Err(status) => Err(zx::Status::from_raw(status)),
        },
        Err(e) => {
            error!("Error sending 'GetCountry' request to phy #{}: {}", phy_id, e);
            Err(zx::Status::INTERNAL)
        }
    }
}

async fn set_country(phys: &PhyMap, req: fidl_svc::SetCountryRequest) -> zx::Status {
    let phy_id = req.phy_id;
    let phy = match phys.get(&req.phy_id) {
        None => return zx::Status::NOT_FOUND,
        Some(p) => p,
    };

    let phy_req = fidl_dev::CountryCode { alpha2: req.alpha2 };
    match phy.proxy.set_country(&phy_req).await {
        Ok(status) => zx::Status::from_raw(status),
        Err(e) => {
            error!("Error sending SetCountry set_country request to phy #{}: {}", phy_id, e);
            zx::Status::INTERNAL
        }
    }
}

async fn clear_country(phys: &PhyMap, req: fidl_svc::ClearCountryRequest) -> zx::Status {
    let phy = match phys.get(&req.phy_id) {
        None => return zx::Status::NOT_FOUND,
        Some(p) => p,
    };

    match phy.proxy.clear_country().await {
        Ok(status) => zx::Status::from_raw(status),
        Err(e) => {
            error!(
                "Error sending ClearCountry clear_country request to phy #{}: {}",
                req.phy_id, e
            );
            zx::Status::INTERNAL
        }
    }
}

async fn set_power_save_mode(phys: &PhyMap, req: fidl_svc::SetPowerSaveModeRequest) -> zx::Status {
    let phy_id = req.phy_id;
    let phy = match phys.get(&req.phy_id) {
        None => return zx::Status::NOT_FOUND,
        Some(p) => p,
    };

    let phy_req = req.ps_mode;
    match phy.proxy.set_power_save_mode(phy_req).await {
        Ok(status) => zx::Status::from_raw(status),
        Err(e) => {
            error!("Error sending SetPowerSaveModeRequest request to phy #{}: {}", phy_id, e);
            zx::Status::INTERNAL
        }
    }
}

async fn get_power_save_mode(
    phys: &PhyMap,
    phy_id: u16,
) -> Result<fidl_svc::GetPowerSaveModeResponse, zx::Status> {
    let phy = phys.get(&phy_id).ok_or(Err(zx::Status::NOT_FOUND))?;
    match phy.proxy.get_power_save_mode().await {
        Ok(result) => match result {
            Ok(resp) => Ok(fidl_svc::GetPowerSaveModeResponse { ps_mode: resp }),
            Err(status) => Err(zx::Status::from_raw(status)),
        },
        Err(e) => {
            error!("Error sending 'GetPowerSaveMode' request to phy #{}: {}", phy_id, e);
            Err(zx::Status::INTERNAL)
        }
    }
}

async fn get_supported_mac_roles(
    phys: &PhyMap,
    id: u16,
) -> Result<Vec<fidl_common::WlanMacRole>, zx::sys::zx_status_t> {
    let phy = phys.get(&id).ok_or(zx::sys::ZX_ERR_NOT_FOUND)?;
    phy.proxy.get_supported_mac_roles().await.map_err(move |fidl_error| {
        error!("get_supported_mac_roles(id = {}): error sending 'GetSupportedMacRoles' request to phy: {}", id, fidl_error);
        zx::sys::ZX_ERR_INTERNAL
    })?.map_err(move |e| {
        let status = zx::Status::from_raw(e);
        error!("get_supported_mac_roles(id = {}): returned an error: {}", id, status);
        status.into_raw()
    })
}

async fn create_iface(
    new_iface_sink: &mpsc::UnboundedSender<NewIface>,
    phys: &PhyMap,
    req: fidl_svc::CreateIfaceRequest,
    iface_counter: &Arc<IfaceCounter>,
    cfg: &wlandevicemonitor_config::Config,
    ifaces_node: &mut fuchsia_inspect::Node,
) -> Result<NewIface, zx::Status> {
    let phy_id = req.phy_id;
    let phy = phys.get(&req.phy_id).ok_or(zx::Status::NOT_FOUND)?;

    // Create the bootstrap channel. This channel is only used for initial communication
    // with the USME device.
    let (usme_bootstrap_client, usme_bootstrap_server) =
        create_endpoints::<fidl_sme::UsmeBootstrapMarker>();
    let usme_bootstrap_proxy = usme_bootstrap_client.into_proxy().map_err(|e| {
        error!("Error creating UsmeBootstrapProxy: {}", e);
        zx::Status::INTERNAL
    })?;

    // Create a GenericSme channel. This channel will be used for continued communication with
    // the USME driver and hence will be persisted.
    let (generic_sme_client, generic_sme_server) = create_endpoints::<fidl_sme::GenericSmeMarker>();
    let generic_sme_proxy = generic_sme_client.into_proxy().map_err(|e| {
        error!("Error creating GenericSmeProxy: {}", e);
        zx::Status::INTERNAL
    })?;

    let legacy_privacy_support = fidl_sme::LegacyPrivacySupport {
        wep_supported: cfg.wep_supported,
        wpa1_supported: cfg.wpa1_supported,
    };

    // Pipeline the GenericSme channel and the legacy privacy support configuration, via
    // the bootstrap channel. This request will be handled after our call to
    // create_iface in the phy proxy, so we cannot await the result yet.
    let bootstrap_result = usme_bootstrap_proxy.start(generic_sme_server, &legacy_privacy_support);

    // Send a CreateIfaceRequest. The vendor device will then spawn a USME device and pass
    // the bootstrap to it. The USME device will then read the config and GenericSme channel,
    // and continue to listen to requests via the GenericSme channel.
    let phy_req = fidl_dev::CreateIfaceRequest {
        role: req.role,
        mlme_channel: Some(usme_bootstrap_server.into_channel()),
        init_sta_addr: req.sta_addr,
    };
    let phy_assigned_iface_id = phy
        .proxy
        .create_iface(phy_req)
        .await
        .map_err(move |e| {
            error!("CreateIface failed: phy {}, fidl error {}", phy_id, e);
            zx::Status::INTERNAL
        })?
        .map_err(move |e| {
            error!("CreateIface failed: phy {}, error {}", phy_id, e);
            zx::Status::ok(e)
        })?;

    let inspect_vmo = std::sync::Arc::new(bootstrap_result.await.map_err(|e| {
        error!("Failed to bootstrap USME: {}", e);
        zx::Status::INTERNAL
    })?);

    let iface_id = iface_counter.next_iface_id() as u16;
    let iface_node_name = std::format!("{}", iface_id);
    let inspect_vmo_clone = Arc::clone(&inspect_vmo);
    let inspect_node = ifaces_node.create_lazy_child(iface_node_name, move || {
        let inspect_vmo_inner_clone = Arc::clone(&inspect_vmo_clone);
        async move {
            let iface_inspector = Inspector::new(
                fuchsia_inspect::InspectorConfig::default()
                    .vmo(inspect_vmo_inner_clone.duplicate_handle(zx::Rights::SAME_RIGHTS)?),
            );
            Ok(iface_inspector)
        }
        .boxed()
    });
    let new_iface = NewIface {
        id: iface_id,
        phy_ownership: device::PhyOwnership { phy_id, phy_assigned_id: phy_assigned_iface_id },
        generic_sme: generic_sme_proxy,
        inspect_node: Some(std::sync::Arc::new(inspect_node)),
        inspect_vmo: Some(inspect_vmo),
    };
    new_iface_sink.unbounded_send(new_iface.clone()).map_err(|e| {
        error!("Failed to register Generic SME event stream with internal handler: {}", e);
        zx::Status::INTERNAL
    })?;

    Ok(new_iface)
}

async fn query_iface(
    ifaces: &IfaceMap,
    id: u16,
) -> Result<fidl_svc::QueryIfaceResponse, zx::Status> {
    info!("query_iface(id = {})", id);
    let iface = ifaces.get(&id).ok_or(zx::Status::NOT_FOUND)?;
    let iface_query_info = iface.generic_sme.query().await.map_err(|_| zx::Status::CANCELED)?;
    let phy_ownership = &iface.phy_ownership;
    Ok(fidl_svc::QueryIfaceResponse {
        role: iface_query_info.role,
        id,
        phy_id: phy_ownership.phy_id,
        phy_assigned_id: phy_ownership.phy_assigned_id,
        sta_addr: iface_query_info.sta_addr,
    })
}

async fn destroy_iface(
    phys: &PhyMap,
    ifaces: &IfaceMap,
    _ifaces_node: fuchsia_inspect::Node,
    id: u16,
) -> Result<(), zx::Status> {
    info!("destroy_iface(id = {})", id);
    let iface = ifaces.get(&id).ok_or(zx::Status::NOT_FOUND)?;
    let phy_ownership = &iface.phy_ownership;
    let phy = match phys.get(&phy_ownership.phy_id) {
        Some(phy) => phy,
        None => {
            ifaces.remove(&id);
            error!(
                "Attempting to remove interface (ID: {}) from non-existent PHY (ID: {})",
                id, phy_ownership.phy_id
            );
            return Err(zx::Status::NOT_FOUND);
        }
    };
    let phy_req = fidl_dev::DestroyIfaceRequest { id: phy_ownership.phy_assigned_id };
    let destroy_iface_result = match phy.proxy.destroy_iface(&phy_req).await {
        Ok(result) => result,
        Err(fidl::Error::ClientChannelClosed { .. }) => {
            warn!("Failed to send 'DestroyIface' request to phy {:?}: client channel closed. Assuming phy removed.", phy_ownership);
            Ok(())
        }
        Err(error) => {
            error!("Error sending 'DestroyIface' request to phy {:?}: {}", phy_ownership, error);
            return Err(zx::Status::INTERNAL);
        }
    };

    // If the removal is successful or the interface cannot be found, update the internal
    // accounting.
    match destroy_iface_result {
        Ok(()) => {
            ifaces.remove(&id);
            zx::Status::ok(zx::sys::ZX_OK)
        }
        Err(status) => {
            match status {
                zx::sys::ZX_ERR_NOT_FOUND => {
                    if ifaces.get_snapshot().contains_key(&id) {
                        info!(
                            "Encountered NOT_FOUND while removing iface #{} potentially due to recovery.",
                            id
                        );
                        ifaces.remove(&id);
                    }
                }
                _ => {}
            }
            zx::Status::ok(status)
        }
    }
}

async fn get_client_sme(
    ifaces: &IfaceMap,
    id: u16,
    sme_server: fidl::endpoints::ServerEnd<fidl_sme::ClientSmeMarker>,
) -> Result<(), zx::Status> {
    info!("get_client_sme(id = {})", id);
    let iface = ifaces.get(&id).ok_or(zx::Status::NOT_FOUND)?;
    let result = iface.generic_sme.get_client_sme(sme_server).await.map_err(|e| {
        error!("Failed to request client SME: {}", e);
        zx::Status::INTERNAL
    })?;
    result.map_err(|e| zx::Status::from_raw(e))
}

async fn get_ap_sme(
    ifaces: &IfaceMap,
    id: u16,
    sme_server: fidl::endpoints::ServerEnd<fidl_sme::ApSmeMarker>,
) -> Result<(), zx::Status> {
    info!("get_ap_sme(id = {})", id);
    let iface = ifaces.get(&id).ok_or(zx::Status::NOT_FOUND)?;
    let result = iface.generic_sme.get_ap_sme(sme_server).await.map_err(|e| {
        error!("Failed to request AP SME: {}", e);
        zx::Status::INTERNAL
    })?;
    result.map_err(|e| zx::Status::from_raw(e))
}

async fn get_sme_telemetry(
    ifaces: &IfaceMap,
    id: u16,
    telemetry_server: fidl::endpoints::ServerEnd<fidl_sme::TelemetryMarker>,
) -> Result<(), zx::Status> {
    info!("get_sme_telemetry(id = {})", id);
    let iface = ifaces.get(&id).ok_or(zx::Status::NOT_FOUND)?;
    let result = iface.generic_sme.get_sme_telemetry(telemetry_server).await.map_err(|e| {
        error!("Failed to request SME telemetry: {}", e);
        zx::Status::INTERNAL
    })?;
    result.map_err(|e| zx::Status::from_raw(e))
}

async fn get_feature_support(
    ifaces: &IfaceMap,
    id: u16,
    feature_support_server: fidl::endpoints::ServerEnd<fidl_sme::FeatureSupportMarker>,
) -> Result<(), zx::Status> {
    info!("get_feature_support(id = {})", id);
    let iface = ifaces.get(&id).ok_or(zx::Status::NOT_FOUND)?;
    let result =
        iface.generic_sme.get_feature_support(feature_support_server).await.map_err(|e| {
            error!("Failed to request feature support: {}", e);
            zx::Status::INTERNAL
        })?;
    result.map_err(|e| zx::Status::from_raw(e))
}

fn into_status_and_opt<T>(r: Result<T, zx::Status>) -> (zx::Status, Option<T>) {
    match r {
        Ok(x) => (zx::Status::OK, Some(x)),
        Err(status) => (status, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::PhyOwnership;
    use fidl::endpoints::{create_proxy, create_proxy_and_stream, ControlHandle};
    use futures::future::BoxFuture;
    use futures::task::Poll;
    use ieee80211::{MacAddrBytes, NULL_ADDR};
    use std::convert::Infallible;
    use std::pin::pin;
    use test_case::test_case;
    use wlan_common::assert_variant;
    use {fidl_fuchsia_wlan_common as fidl_wlan_common, fuchsia_async as fasync};

    struct TestValues {
        monitor_proxy: fidl_svc::DeviceMonitorProxy,
        monitor_req_stream: fidl_svc::DeviceMonitorRequestStream,
        phys: Arc<PhyMap>,
        ifaces: Arc<IfaceMap>,
        watcher_service: watcher_service::WatcherService<PhyDevice, IfaceDevice>,
        watcher_fut: BoxFuture<'static, Result<Infallible, Error>>,
        new_iface_stream: mpsc::UnboundedReceiver<NewIface>,
        new_iface_sink: mpsc::UnboundedSender<NewIface>,
        iface_counter: Arc<IfaceCounter>,
        inspector: Inspector,
        ifaces_node: fuchsia_inspect::Node,
    }

    fn test_setup() -> TestValues {
        let (monitor_proxy, requests) = create_proxy::<fidl_svc::DeviceMonitorMarker>()
            .expect("failed to create DeviceMonitor proxy");
        let monitor_req_stream = requests.into_stream().expect("failed to create request stream");
        let (phys, phy_events) = PhyMap::new();
        let phys = Arc::new(phys);

        let (ifaces, iface_events) = IfaceMap::new();
        let ifaces = Arc::new(ifaces);

        let (watcher_service, watcher_fut) =
            watcher_service::serve_watchers(phys.clone(), ifaces.clone(), phy_events, iface_events);

        let (new_iface_sink, new_iface_stream) = mpsc::unbounded();

        let iface_counter = Arc::new(IfaceCounter::new());

        let inspector = Inspector::default();
        let ifaces_node = inspector.root().create_child("devices");

        TestValues {
            monitor_proxy,
            monitor_req_stream,
            phys,
            ifaces,
            watcher_service,
            watcher_fut: Box::pin(watcher_fut),
            new_iface_stream: new_iface_stream,
            new_iface_sink,
            iface_counter,
            inspector,
            ifaces_node,
        }
    }

    fn fake_phy() -> (PhyDevice, fidl_dev::PhyRequestStream) {
        let (proxy, server) =
            create_proxy::<fidl_dev::PhyMarker>().expect("fake_phy: create_proxy() failed");
        let stream = server.into_stream().expect("fake_phy: failed to create stream");
        (PhyDevice { proxy, device_path: String::from("/test/path") }, stream)
    }

    fn fake_alpha2() -> [u8; 2] {
        let mut alpha2: [u8; 2] = [0, 0];
        alpha2.copy_from_slice("MX".as_bytes());
        alpha2
    }

    fn fake_wlandevicemonitor_config() -> wlandevicemonitor_config::Config {
        wlandevicemonitor_config::Config { wep_supported: false, wpa1_supported: false }
    }

    #[fuchsia::test]
    fn test_list_phys() {
        let mut exec = fasync::TestExecutor::new();
        let test_values = test_setup();
        let service_fut = serve_monitor_requests(
            test_values.monitor_req_stream,
            test_values.phys.clone(),
            test_values.ifaces.clone(),
            test_values.watcher_service,
            test_values.new_iface_sink,
            test_values.iface_counter,
            test_values.ifaces_node,
            fake_wlandevicemonitor_config(),
        );
        let mut service_fut = pin!(service_fut);

        assert_variant!(exec.run_until_stalled(&mut service_fut), Poll::Pending);

        // Request the list of available PHYs.
        let list_fut = test_values.monitor_proxy.list_phys();
        let mut list_fut = pin!(list_fut);
        assert_variant!(exec.run_until_stalled(&mut list_fut), Poll::Pending);

        // Progress the service loop.
        assert_variant!(exec.run_until_stalled(&mut service_fut), Poll::Pending);

        // The future to list the PHYs should complete and no PHYs should be present.
        assert_variant!(exec.run_until_stalled(&mut list_fut),Poll::Ready(Ok(phys)) => {
            assert!(phys.is_empty())
        });

        // Add a PHY to the PhyMap.
        let (phy, _req_stream) = fake_phy();
        test_values.phys.insert(0, phy);

        // Request the list of available PHYs.
        let list_fut = test_values.monitor_proxy.list_phys();
        let mut list_fut = pin!(list_fut);
        assert_variant!(exec.run_until_stalled(&mut list_fut), Poll::Pending);

        // Progress the service loop.
        assert_variant!(exec.run_until_stalled(&mut service_fut), Poll::Pending);

        // The future to list the PHYs should complete and the PHY should be present.
        assert_variant!(exec.run_until_stalled(&mut list_fut), Poll::Ready(Ok(phys)) => {
            assert_eq!(vec![0u16], phys);
        });

        // Remove the PHY from the map.
        test_values.phys.remove(&0);

        // Request the list of available PHYs.
        let list_fut = test_values.monitor_proxy.list_phys();
        let mut list_fut = pin!(list_fut);
        assert_variant!(exec.run_until_stalled(&mut list_fut), Poll::Pending);

        // Progress the service loop.
        assert_variant!(exec.run_until_stalled(&mut service_fut), Poll::Pending);

        // The future to list the PHYs should complete and no PHYs should be present.
        assert_variant!(exec.run_until_stalled(&mut list_fut), Poll::Ready(Ok(phys)) => {
            assert!(phys.is_empty())
        });
    }

    #[fuchsia::test]
    fn test_list_ifaces() {
        let mut exec = fasync::TestExecutor::new();
        let test_values = test_setup();
        let service_fut = serve_monitor_requests(
            test_values.monitor_req_stream,
            test_values.phys.clone(),
            test_values.ifaces.clone(),
            test_values.watcher_service,
            test_values.new_iface_sink,
            test_values.iface_counter,
            test_values.ifaces_node,
            fake_wlandevicemonitor_config(),
        );
        let mut service_fut = pin!(service_fut);

        assert_variant!(exec.run_until_stalled(&mut service_fut), Poll::Pending);

        // Request the list of available ifaces.
        let list_fut = test_values.monitor_proxy.list_ifaces();
        let mut list_fut = pin!(list_fut);
        assert_variant!(exec.run_until_stalled(&mut list_fut), Poll::Pending);

        // Progress the service loop.
        assert_variant!(exec.run_until_stalled(&mut service_fut), Poll::Pending);

        // The future to list the ifaces should complete and no ifaces should be present.
        assert_variant!(exec.run_until_stalled(&mut list_fut),Poll::Ready(Ok(ifaces)) => {
            assert!(ifaces.is_empty())
        });

        // Add a fake iface.
        let (generic_sme, _) =
            create_proxy::<fidl_sme::GenericSmeMarker>().expect("Failed to create generic sme");
        let fake_iface = IfaceDevice {
            phy_ownership: PhyOwnership { phy_id: 0, phy_assigned_id: 0 },
            generic_sme,
            inspect_node: None,
            inspect_vmo: None,
        };
        test_values.ifaces.insert(0, fake_iface);

        // Request the list of available ifaces.
        let list_fut = test_values.monitor_proxy.list_ifaces();
        let mut list_fut = pin!(list_fut);
        assert_variant!(exec.run_until_stalled(&mut list_fut), Poll::Pending);

        // Progress the service loop.
        assert_variant!(exec.run_until_stalled(&mut service_fut), Poll::Pending);

        // The future to list the ifaces should complete and the iface should be present.
        assert_variant!(exec.run_until_stalled(&mut list_fut), Poll::Ready(Ok(ifaces)) => {
            assert_eq!(vec![0u16], ifaces);
        });
    }

    #[fuchsia::test]
    fn test_get_dev_path_success() {
        let mut exec = fasync::TestExecutor::new();
        let test_values = test_setup();
        let (phy, _) = fake_phy();

        let expected_path = phy.device_path.clone();
        test_values.phys.insert(10u16, phy);

        let service_fut = serve_monitor_requests(
            test_values.monitor_req_stream,
            test_values.phys,
            test_values.ifaces,
            test_values.watcher_service,
            test_values.new_iface_sink,
            test_values.iface_counter,
            test_values.ifaces_node,
            fake_wlandevicemonitor_config(),
        );
        let mut service_fut = pin!(service_fut);
        assert_variant!(exec.run_until_stalled(&mut service_fut), Poll::Pending);

        // Initiate a GetDevPath request. The returned future should not be able
        // to produce a result immediately
        let query_fut = test_values.monitor_proxy.get_dev_path(10u16);
        let mut query_fut = pin!(query_fut);
        assert_variant!(exec.run_until_stalled(&mut query_fut), Poll::Pending);
        assert_variant!(exec.run_until_stalled(&mut service_fut), Poll::Pending);

        // Our original future should complete, and return the dev path.
        assert_variant!(
            exec.run_until_stalled(&mut query_fut),
            Poll::Ready(Ok(Some(path))) => {
                assert_eq!(path, expected_path);
            }
        );
    }

    #[fuchsia::test]
    fn test_get_dev_path_phy_not_found() {
        let mut exec = fasync::TestExecutor::new();
        let test_values = test_setup();
        let service_fut = serve_monitor_requests(
            test_values.monitor_req_stream,
            test_values.phys,
            test_values.ifaces.clone(),
            test_values.watcher_service,
            test_values.new_iface_sink,
            test_values.iface_counter,
            test_values.ifaces_node,
            fake_wlandevicemonitor_config(),
        );
        let mut service_fut = pin!(service_fut);

        assert_variant!(exec.run_until_stalled(&mut service_fut), Poll::Pending);

        // Query a PHY's dev path.
        let query_fut = test_values.monitor_proxy.get_dev_path(0);
        let mut query_fut = pin!(query_fut);
        assert_variant!(exec.run_until_stalled(&mut query_fut), Poll::Pending);

        // Progress the service loop.
        assert_variant!(exec.run_until_stalled(&mut service_fut), Poll::Pending);

        // The attempt to query the PHY's information should fail.
        assert_variant!(exec.run_until_stalled(&mut query_fut), Poll::Ready(Ok(None)));
    }

    #[fuchsia::test]
    fn test_get_mac_roles_success() {
        let mut exec = fasync::TestExecutor::new();
        let test_values = test_setup();
        let (phy, mut phy_stream) = fake_phy();
        test_values.phys.insert(10u16, phy);

        let service_fut = serve_monitor_requests(
            test_values.monitor_req_stream,
            test_values.phys,
            test_values.ifaces,
            test_values.watcher_service,
            test_values.new_iface_sink,
            test_values.iface_counter,
            test_values.ifaces_node,
            fake_wlandevicemonitor_config(),
        );
        let mut service_fut = pin!(service_fut);
        assert_variant!(exec.run_until_stalled(&mut service_fut), Poll::Pending);

        // Initiate a GetWlanMacRoles request. The returned future should not be able
        // to produce a result immediately
        let get_supported_mac_roles_fut = test_values.monitor_proxy.get_supported_mac_roles(10u16);
        let mut get_supported_mac_roles_fut = pin!(get_supported_mac_roles_fut);
        assert_variant!(exec.run_until_stalled(&mut get_supported_mac_roles_fut), Poll::Pending);

        // The call above should trigger a Query message to the phy.
        // Pretend that we are the phy and read the message from the other side.
        assert_variant!(exec.run_until_stalled(&mut service_fut), Poll::Pending);
        let responder = assert_variant!(exec.run_until_stalled(&mut phy_stream.next()),
            Poll::Ready(Some(Ok(fidl_dev::PhyRequest::GetSupportedMacRoles { responder }))) => responder
        );

        // Reply with a fake phy info
        responder
            .send(Ok(&[fidl_wlan_common::WlanMacRole::Client]))
            .expect("failed to send QueryResponse");
        assert_variant!(exec.run_until_stalled(&mut service_fut), Poll::Pending);

        // Our original future should complete now and the client role should be reported.
        assert_variant!(
            exec.run_until_stalled(&mut get_supported_mac_roles_fut),
            Poll::Ready(Ok(Ok(supported_mac_roles))) => {
                assert_eq!(supported_mac_roles, vec![fidl_wlan_common::WlanMacRole::Client]);
            }
        );
    }

    #[fuchsia::test]
    fn test_get_mac_roles_phy_not_found() {
        let mut exec = fasync::TestExecutor::new();
        let test_values = test_setup();
        let service_fut = serve_monitor_requests(
            test_values.monitor_req_stream,
            test_values.phys,
            test_values.ifaces.clone(),
            test_values.watcher_service,
            test_values.new_iface_sink,
            test_values.iface_counter,
            test_values.ifaces_node,
            fake_wlandevicemonitor_config(),
        );
        let mut service_fut = pin!(service_fut);

        assert_variant!(exec.run_until_stalled(&mut service_fut), Poll::Pending);

        // Query a PHY's dev path.
        let get_supported_mac_roles_fut = test_values.monitor_proxy.get_supported_mac_roles(0);
        let mut get_supported_mac_roles_fut = pin!(get_supported_mac_roles_fut);
        assert_variant!(exec.run_until_stalled(&mut get_supported_mac_roles_fut), Poll::Pending);

        // Progress the service loop.
        assert_variant!(exec.run_until_stalled(&mut service_fut), Poll::Pending);

        // The attempt to query the PHY's information should fail.
        assert_variant!(
            exec.run_until_stalled(&mut get_supported_mac_roles_fut),
            Poll::Ready(Ok(Err(zx::sys::ZX_ERR_NOT_FOUND)))
        );
    }

    #[fuchsia::test]
    fn test_watch_devices_add_remove_phy() {
        let mut exec = fasync::TestExecutor::new();
        let test_values = test_setup();
        let watcher_fut = test_values.watcher_fut;
        let mut watcher_fut = pin!(watcher_fut);

        let service_fut = serve_monitor_requests(
            test_values.monitor_req_stream,
            test_values.phys.clone(),
            test_values.ifaces.clone(),
            test_values.watcher_service,
            test_values.new_iface_sink,
            test_values.iface_counter,
            test_values.ifaces_node,
            fake_wlandevicemonitor_config(),
        );
        let mut service_fut = pin!(service_fut);

        assert_variant!(exec.run_until_stalled(&mut service_fut), Poll::Pending);

        // Watch for new devices.
        let (watcher_proxy, watcher_server_end) =
            fidl::endpoints::create_proxy().expect("failed to create watcher proxy");
        test_values
            .monitor_proxy
            .watch_devices(watcher_server_end)
            .expect("failed to watch devices");

        // Progress the service loop.
        assert_variant!(exec.run_until_stalled(&mut service_fut), Poll::Pending);

        // Initially there should be no devices and the future should be pending.
        let mut events_fut = watcher_proxy.take_event_stream();
        let next_fut = events_fut.try_next();
        let mut next_fut = pin!(next_fut);
        assert_variant!(exec.run_until_stalled(&mut next_fut), Poll::Pending);

        // Add a PHY and make sure the update is received.
        let (phy, _phy_stream) = fake_phy();
        test_values.phys.insert(0, phy);
        assert_variant!(exec.run_until_stalled(&mut watcher_fut), Poll::Pending);
        assert_variant!(
            exec.run_until_stalled(&mut next_fut),
            Poll::Ready(Ok(Some(fidl_svc::DeviceWatcherEvent::OnPhyAdded { phy_id: 0 })))
        );

        // Remove the PHY and make sure the update is received.
        test_values.phys.remove(&0);
        assert_variant!(exec.run_until_stalled(&mut watcher_fut), Poll::Pending);
        assert_variant!(
            exec.run_until_stalled(&mut next_fut),
            Poll::Ready(Ok(Some(fidl_svc::DeviceWatcherEvent::OnPhyRemoved { phy_id: 0 })))
        );
    }

    #[fuchsia::test]
    fn test_watch_devices_remove_existing_phy() {
        let mut exec = fasync::TestExecutor::new();
        let test_values = test_setup();
        let watcher_fut = test_values.watcher_fut;
        let mut watcher_fut = pin!(watcher_fut);

        let service_fut = serve_monitor_requests(
            test_values.monitor_req_stream,
            test_values.phys.clone(),
            test_values.ifaces.clone(),
            test_values.watcher_service,
            test_values.new_iface_sink,
            test_values.iface_counter,
            test_values.ifaces_node,
            fake_wlandevicemonitor_config(),
        );
        let mut service_fut = pin!(service_fut);

        assert_variant!(exec.run_until_stalled(&mut service_fut), Poll::Pending);

        // Add a PHY before beginning to watch for devices.
        let (phy, _phy_stream) = fake_phy();
        test_values.phys.insert(0, phy);
        assert_variant!(exec.run_until_stalled(&mut watcher_fut), Poll::Pending);

        // Watch for new devices.
        let (watcher_proxy, watcher_server_end) =
            fidl::endpoints::create_proxy().expect("failed to create watcher proxy");
        test_values
            .monitor_proxy
            .watch_devices(watcher_server_end)
            .expect("failed to watch devices");

        // Progress the service loop.
        assert_variant!(exec.run_until_stalled(&mut service_fut), Poll::Pending);

        // Start listening for device events.
        let mut events_fut = watcher_proxy.take_event_stream();
        let next_fut = events_fut.try_next();
        let mut next_fut = pin!(next_fut);
        assert_variant!(exec.run_until_stalled(&mut next_fut), Poll::Pending);

        // We should be notified of the existing PHY.
        assert_variant!(exec.run_until_stalled(&mut watcher_fut), Poll::Pending);
        assert_variant!(
            exec.run_until_stalled(&mut next_fut),
            Poll::Ready(Ok(Some(fidl_svc::DeviceWatcherEvent::OnPhyAdded { phy_id: 0 })))
        );

        // Remove the PHY and make sure the update is received.
        test_values.phys.remove(&0);
        assert_variant!(exec.run_until_stalled(&mut watcher_fut), Poll::Pending);
        assert_variant!(
            exec.run_until_stalled(&mut next_fut),
            Poll::Ready(Ok(Some(fidl_svc::DeviceWatcherEvent::OnPhyRemoved { phy_id: 0 })))
        );
    }

    #[fuchsia::test]
    fn test_watch_devices_add_remove_iface() {
        let mut exec = fasync::TestExecutor::new();
        let test_values = test_setup();
        let watcher_fut = test_values.watcher_fut;
        let mut watcher_fut = pin!(watcher_fut);

        let service_fut = serve_monitor_requests(
            test_values.monitor_req_stream,
            test_values.phys.clone(),
            test_values.ifaces.clone(),
            test_values.watcher_service,
            test_values.new_iface_sink,
            test_values.iface_counter,
            test_values.ifaces_node,
            fake_wlandevicemonitor_config(),
        );
        let mut service_fut = pin!(service_fut);

        assert_variant!(exec.run_until_stalled(&mut service_fut), Poll::Pending);

        // Watch for new devices.
        let (watcher_proxy, watcher_server_end) =
            fidl::endpoints::create_proxy().expect("failed to create watcher proxy");
        test_values
            .monitor_proxy
            .watch_devices(watcher_server_end)
            .expect("failed to watch devices");

        // Progress the service loop.
        assert_variant!(exec.run_until_stalled(&mut service_fut), Poll::Pending);

        // Initially there should be no devices and the future should be pending.
        let mut events_fut = watcher_proxy.take_event_stream();
        let next_fut = events_fut.try_next();
        let mut next_fut = pin!(next_fut);
        assert_variant!(exec.run_until_stalled(&mut next_fut), Poll::Pending);

        // Create a generic SME proxy but drop the server since we won't use it.
        let (generic_sme, _) = create_proxy::<fidl_sme::GenericSmeMarker>()
            .expect("Failed to create generic SME proxy");
        // Add an interface and make sure the update is received.
        let fake_iface = IfaceDevice {
            phy_ownership: PhyOwnership { phy_id: 0, phy_assigned_id: 0 },
            generic_sme,
            inspect_node: None,
            inspect_vmo: None,
        };
        test_values.ifaces.insert(0, fake_iface);
        assert_variant!(exec.run_until_stalled(&mut watcher_fut), Poll::Pending);
        assert_variant!(
            exec.run_until_stalled(&mut next_fut),
            Poll::Ready(Ok(Some(fidl_svc::DeviceWatcherEvent::OnIfaceAdded { iface_id: 0 })))
        );

        // Remove the PHY and make sure the update is received.
        test_values.ifaces.remove(&0);
        assert_variant!(exec.run_until_stalled(&mut watcher_fut), Poll::Pending);
        assert_variant!(
            exec.run_until_stalled(&mut next_fut),
            Poll::Ready(Ok(Some(fidl_svc::DeviceWatcherEvent::OnIfaceRemoved { iface_id: 0 })))
        );
    }

    #[fuchsia::test]
    fn test_watch_devices_remove_existing_iface() {
        let mut exec = fasync::TestExecutor::new();
        let test_values = test_setup();
        let watcher_fut = test_values.watcher_fut;
        let mut watcher_fut = pin!(watcher_fut);

        let service_fut = serve_monitor_requests(
            test_values.monitor_req_stream,
            test_values.phys.clone(),
            test_values.ifaces.clone(),
            test_values.watcher_service,
            test_values.new_iface_sink,
            test_values.iface_counter,
            test_values.ifaces_node,
            fake_wlandevicemonitor_config(),
        );
        let mut service_fut = pin!(service_fut);

        assert_variant!(exec.run_until_stalled(&mut service_fut), Poll::Pending);

        // Create a generic SME proxy but drop the server since we won't use it.
        let (generic_sme, _) = create_proxy::<fidl_sme::GenericSmeMarker>()
            .expect("Failed to create generic SME proxy");
        // Add an interface before beginning to watch for devices.
        let fake_iface = IfaceDevice {
            phy_ownership: PhyOwnership { phy_id: 0, phy_assigned_id: 0 },
            generic_sme,
            inspect_node: None,
            inspect_vmo: None,
        };
        test_values.ifaces.insert(0, fake_iface);
        assert_variant!(exec.run_until_stalled(&mut watcher_fut), Poll::Pending);

        // Watch for new devices.
        let (watcher_proxy, watcher_server_end) =
            fidl::endpoints::create_proxy().expect("failed to create watcher proxy");
        test_values
            .monitor_proxy
            .watch_devices(watcher_server_end)
            .expect("failed to watch devices");

        // Progress the service loop.
        assert_variant!(exec.run_until_stalled(&mut service_fut), Poll::Pending);

        // Start listening for device events.
        let mut events_fut = watcher_proxy.take_event_stream();
        let next_fut = events_fut.try_next();
        let mut next_fut = pin!(next_fut);
        assert_variant!(exec.run_until_stalled(&mut next_fut), Poll::Pending);

        // We should be notified of the existing interface.
        assert_variant!(exec.run_until_stalled(&mut watcher_fut), Poll::Pending);
        assert_variant!(
            exec.run_until_stalled(&mut next_fut),
            Poll::Ready(Ok(Some(fidl_svc::DeviceWatcherEvent::OnIfaceAdded { iface_id: 0 })))
        );

        // Remove the interface and make sure the update is received.
        test_values.ifaces.remove(&0);
        assert_variant!(exec.run_until_stalled(&mut watcher_fut), Poll::Pending);
        assert_variant!(
            exec.run_until_stalled(&mut next_fut),
            Poll::Ready(Ok(Some(fidl_svc::DeviceWatcherEvent::OnIfaceRemoved { iface_id: 0 })))
        );
    }

    #[fuchsia::test]
    fn test_set_country_succeeds() {
        // Setup environment
        let mut exec = fasync::TestExecutor::new();
        let test_values = test_setup();
        let (phy, mut phy_stream) = fake_phy();
        let phy_id = 10u16;
        test_values.phys.insert(phy_id, phy);
        let alpha2 = fake_alpha2();

        // Initiate a QueryPhy request. The returned future should not be able
        // to produce a result immediately
        // Issue service.fidl::SetCountryRequest()
        let req_msg = fidl_svc::SetCountryRequest { phy_id, alpha2: alpha2.clone() };
        let req_fut = super::set_country(&test_values.phys, req_msg);
        let mut req_fut = pin!(req_fut);
        assert_eq!(Poll::Pending, exec.run_until_stalled(&mut req_fut));

        assert_variant!(exec.run_until_stalled(&mut phy_stream.next()),
            Poll::Ready(Some(Ok(fidl_dev::PhyRequest::SetCountry { req, responder }))) => {
                assert_eq!(req.alpha2, alpha2.clone());
                // Pretend to be a WLAN PHY to return the result.
                responder.send(zx::Status::OK.into_raw())
                    .expect("failed to send the response to SetCountry");
            }
        );

        // req_fut should have completed by now. Test the result.
        assert_eq!(exec.run_until_stalled(&mut req_fut), Poll::Ready(zx::Status::OK));
    }

    #[fuchsia::test]
    fn test_set_country_fails() {
        // Setup environment
        let mut exec = fasync::TestExecutor::new();
        let test_values = test_setup();
        let (phy, mut phy_stream) = fake_phy();
        let phy_id = 10u16;
        test_values.phys.insert(phy_id, phy);
        let alpha2 = fake_alpha2();

        // Initiate a QueryPhy request. The returned future should not be able
        // to produce a result immediately
        // Issue service.fidl::SetCountryRequest()
        let req_msg = fidl_svc::SetCountryRequest { phy_id, alpha2: alpha2.clone() };
        let req_fut = super::set_country(&test_values.phys, req_msg);
        let mut req_fut = pin!(req_fut);
        assert_eq!(Poll::Pending, exec.run_until_stalled(&mut req_fut));

        let (req, responder) = assert_variant!(exec.run_until_stalled(&mut phy_stream.next()),
            Poll::Ready(Some(Ok(fidl_dev::PhyRequest::SetCountry { req, responder }))) => (req, responder)
        );
        assert_eq!(req.alpha2, alpha2.clone());

        // Failure case #1: WLAN PHY not responding
        assert_eq!(Poll::Pending, exec.run_until_stalled(&mut req_fut));

        // Failure case #2: WLAN PHY has not implemented the feature.
        assert_eq!(Poll::Pending, exec.run_until_stalled(&mut req_fut));
        let resp = zx::Status::NOT_SUPPORTED.into_raw();
        responder.send(resp).expect("failed to send the response to SetCountry");
        assert_eq!(Poll::Ready(zx::Status::NOT_SUPPORTED), exec.run_until_stalled(&mut req_fut));
    }

    #[fuchsia::test]
    fn test_get_country_succeeds() {
        // Setup environment
        let mut exec = fasync::TestExecutor::new();
        let test_values = test_setup();

        let (phy, mut phy_stream) = fake_phy();
        let phy_id = 10u16;
        test_values.phys.insert(phy_id, phy);
        let alpha2 = fake_alpha2();

        // Initiate a QueryPhy request. The returned future should not be able
        // to produce a result immediately
        // Issue service.fidl::GetCountryRequest()
        let req_fut = super::get_country(&test_values.phys, phy_id);
        let mut req_fut = pin!(req_fut);
        assert_eq!(Poll::Pending, exec.run_until_stalled(&mut req_fut));

        assert_variant!(exec.run_until_stalled(&mut phy_stream.next()),
            Poll::Ready(Some(Ok(fidl_dev::PhyRequest::GetCountry { responder }))) => {
                // Pretend to be a WLAN PHY to return the result.
                responder.send(
                    Ok(&fidl_dev::CountryCode { alpha2 })
                ).expect("failed to send the response to GetCountry");
            }
        );

        assert_eq!(
            exec.run_until_stalled(&mut req_fut),
            Poll::Ready(Ok(fidl_svc::GetCountryResponse { alpha2 }))
        );
    }

    #[fuchsia::test]
    fn test_get_country_fails() {
        // Setup environment
        let mut exec = fasync::TestExecutor::new();
        let test_values = test_setup();
        let (phy, mut phy_stream) = fake_phy();
        let phy_id = 10u16;
        test_values.phys.insert(phy_id, phy);

        // Initiate a QueryPhy request. The returned future should not be able
        // to produce a result immediately
        // Issue service.fidl::GetCountryRequest()
        let req_fut = super::get_country(&test_values.phys, phy_id);
        let mut req_fut = pin!(req_fut);
        assert_eq!(Poll::Pending, exec.run_until_stalled(&mut req_fut));

        assert_variant!(exec.run_until_stalled(&mut phy_stream.next()),
            Poll::Ready(Some(Ok(fidl_dev::PhyRequest::GetCountry { responder }))) => {
                // Pretend to be a WLAN PHY to return the result.
                // Right now the returned country code is not optional, so we just return garbage.
                responder.send(Err(zx::Status::NOT_SUPPORTED.into_raw()))
                    .expect("failed to send the response to GetCountry");
            }
        );

        assert_variant!(exec.run_until_stalled(&mut req_fut), Poll::Ready(Err(_)));
    }

    #[fuchsia::test]
    fn test_clear_country_succeeds() {
        // Setup environment
        let mut exec = fasync::TestExecutor::new();
        let test_values = test_setup();
        let (phy, mut phy_stream) = fake_phy();
        let phy_id = 10u16;
        test_values.phys.insert(phy_id, phy);

        // Initiate a QueryPhy request. The returned future should not be able
        // to produce a result immediately
        // Issue service.fidl::ClearCountryRequest()
        let req_msg = fidl_svc::ClearCountryRequest { phy_id };
        let req_fut = super::clear_country(&test_values.phys, req_msg);
        let mut req_fut = pin!(req_fut);
        assert_eq!(Poll::Pending, exec.run_until_stalled(&mut req_fut));

        assert_variant!(exec.run_until_stalled(&mut phy_stream.next()),
            Poll::Ready(Some(Ok(fidl_dev::PhyRequest::ClearCountry { responder }))) => {
                // Pretend to be a WLAN PHY to return the result.
                responder.send(zx::Status::OK.into_raw())
                    .expect("failed to send the response to ClearCountry");
            }
        );

        // req_fut should have completed by now. Test the result.
        assert_eq!(exec.run_until_stalled(&mut req_fut), Poll::Ready(zx::Status::OK));
    }

    #[fuchsia::test]
    fn test_clear_country_fails() {
        // Setup environment
        let mut exec = fasync::TestExecutor::new();
        let test_values = test_setup();
        let (phy, mut phy_stream) = fake_phy();
        let phy_id = 10u16;
        test_values.phys.insert(phy_id, phy);

        // Initiate a QueryPhy request. The returned future should not be able
        // to produce a result immediately
        // Issue service.fidl::ClearCountryRequest()
        let req_msg = fidl_svc::ClearCountryRequest { phy_id };
        let req_fut = super::clear_country(&test_values.phys, req_msg);
        let mut req_fut = pin!(req_fut);
        assert_eq!(Poll::Pending, exec.run_until_stalled(&mut req_fut));

        let responder = assert_variant!(exec.run_until_stalled(&mut phy_stream.next()),
            Poll::Ready(Some(Ok(fidl_dev::PhyRequest::ClearCountry { responder }))) => responder
        );

        // Failure case #1: WLAN PHY not responding
        assert_eq!(Poll::Pending, exec.run_until_stalled(&mut req_fut));

        // Failure case #2: WLAN PHY has not implemented the feature.
        assert_eq!(Poll::Pending, exec.run_until_stalled(&mut req_fut));
        let resp = zx::Status::NOT_SUPPORTED.into_raw();
        responder.send(resp).expect("failed to send the response to ClearCountry");
        assert_eq!(Poll::Ready(zx::Status::NOT_SUPPORTED), exec.run_until_stalled(&mut req_fut));
    }

    #[fuchsia::test]
    fn test_set_power_save_mode_succeeds() {
        // Setup environment
        let mut exec = fasync::TestExecutor::new();
        let test_values = test_setup();
        let (phy, mut phy_stream) = fake_phy();
        let phy_id = 10u16;
        test_values.phys.insert(phy_id, phy);

        // Initiate a QueryPhy request. The returned future should not be able
        // to produce a result immediately
        // Issue service.fidl::SetPowerSaveModeRequest()
        let req_msg = fidl_svc::SetPowerSaveModeRequest {
            phy_id,
            ps_mode: fidl_wlan_common::PowerSaveType::PsModeBalanced,
        };
        let req_fut = super::set_power_save_mode(&test_values.phys, req_msg);
        let mut req_fut = pin!(req_fut);
        assert_eq!(Poll::Pending, exec.run_until_stalled(&mut req_fut));

        assert_variant!(exec.run_until_stalled(&mut phy_stream.next()),
            Poll::Ready(Some(Ok(fidl_dev::PhyRequest::SetPowerSaveMode { req, responder }))) => {
                assert_eq!(req, fidl_wlan_common::PowerSaveType::PsModeBalanced);
                // Pretend to be a WLAN PHY to return the result.
                responder.send(zx::Status::OK.into_raw())
                    .expect("failed to send the response to SetPowerSaveModeRequest");
            }
        );

        // req_fut should have completed by now. Test the result.
        assert_eq!(exec.run_until_stalled(&mut req_fut), Poll::Ready(zx::Status::OK));
    }

    #[fuchsia::test]
    fn test_set_power_save_mode_fails() {
        // Setup environment
        let mut exec = fasync::TestExecutor::new();
        let test_values = test_setup();
        let (phy, mut phy_stream) = fake_phy();
        let phy_id = 10u16;
        test_values.phys.insert(phy_id, phy);

        // Initiate a QueryPhy request. The returned future should not be able
        // to produce a result immediately
        // Issue service.fidl::SetPowerSaveModeRequest()
        let req_msg = fidl_svc::SetPowerSaveModeRequest {
            phy_id,
            ps_mode: fidl_wlan_common::PowerSaveType::PsModeLowPower,
        };
        let req_fut = super::set_power_save_mode(&test_values.phys, req_msg);
        let mut req_fut = pin!(req_fut);
        assert_eq!(Poll::Pending, exec.run_until_stalled(&mut req_fut));

        let (req, responder) = assert_variant!(exec.run_until_stalled(&mut phy_stream.next()),
            Poll::Ready(Some(Ok(fidl_dev::PhyRequest::SetPowerSaveMode { req, responder }))) => (req, responder)
        );
        assert_eq!(req, fidl_wlan_common::PowerSaveType::PsModeLowPower);

        // Failure case #1: WLAN PHY not responding
        assert_eq!(Poll::Pending, exec.run_until_stalled(&mut req_fut));

        // Failure case #2: WLAN PHY has not implemented the feature.
        assert_eq!(Poll::Pending, exec.run_until_stalled(&mut req_fut));
        let resp = zx::Status::NOT_SUPPORTED.into_raw();
        responder.send(resp).expect("failed to send the response to SetPowerSaveMode");
        assert_eq!(Poll::Ready(zx::Status::NOT_SUPPORTED), exec.run_until_stalled(&mut req_fut));
    }

    #[fuchsia::test]
    fn test_get_power_save_mode_succeeds() {
        // Setup environment
        let mut exec = fasync::TestExecutor::new();
        let test_values = test_setup();

        let (phy, mut phy_stream) = fake_phy();
        let phy_id = 10u16;
        test_values.phys.insert(phy_id, phy);

        // Initiate a QueryPhy request. The returned future should not be able
        // to produce a result immediately
        // Issue service.fidl::SetCountryRequest()
        let req_fut = super::get_power_save_mode(&test_values.phys, phy_id);
        let mut req_fut = pin!(req_fut);
        assert_eq!(Poll::Pending, exec.run_until_stalled(&mut req_fut));

        assert_variant!(exec.run_until_stalled(&mut phy_stream.next()),
            Poll::Ready(Some(Ok(fidl_dev::PhyRequest::GetPowerSaveMode { responder }))) => {
                // Pretend to be a WLAN PHY to return the result.
                responder.send(
                    Ok(fidl_wlan_common::PowerSaveType::PsModePerformance)
                ).expect("failed to send the response to SetPowerSaveMode");
            }
        );

        assert_eq!(
            exec.run_until_stalled(&mut req_fut),
            Poll::Ready(Ok(fidl_svc::GetPowerSaveModeResponse {
                ps_mode: fidl_wlan_common::PowerSaveType::PsModePerformance
            }))
        );
    }

    #[fuchsia::test]
    fn test_get_power_save_mode_fails() {
        // Setup environment
        let mut exec = fasync::TestExecutor::new();
        let test_values = test_setup();
        let (phy, mut phy_stream) = fake_phy();
        let phy_id = 10u16;
        test_values.phys.insert(phy_id, phy);

        // Initiate a QueryPhy request. The returned future should not be able
        // to produce a result immediately
        // Issue service.fidl::GetCountryRequest()
        let req_fut = super::get_power_save_mode(&test_values.phys, phy_id);
        let mut req_fut = pin!(req_fut);
        assert_eq!(Poll::Pending, exec.run_until_stalled(&mut req_fut));

        assert_variant!(exec.run_until_stalled(&mut phy_stream.next()),
            Poll::Ready(Some(Ok(fidl_dev::PhyRequest::GetPowerSaveMode { responder }))) => {
                // Pretend to be a WLAN PHY to return the result.
                // Right now the returned country code is not optional, so we just return garbage.
                responder.send(Err(zx::Status::NOT_SUPPORTED.into_raw()))
                    .expect("failed to send the response to GetPowerSaveMode");
            }
        );

        assert_variant!(exec.run_until_stalled(&mut req_fut), Poll::Ready(Err(_)));
    }

    #[fuchsia::test]
    fn test_iface_counter() {
        let iface_counter = IfaceCounter::new();
        assert_eq!(0, iface_counter.next_iface_id());
        assert_eq!(1, iface_counter.next_iface_id());
        assert_eq!(2, iface_counter.next_iface_id());
        assert_eq!(3, iface_counter.next_iface_id());
    }

    #[test_case(false, false; "CreateIface without MAC succeeds")]
    #[test_case(true, false; "CreateIface with MAC succeeds")]
    #[test_case(false, true; "CreateIface fails on interface creation")]
    fn test_create_iface(with_mac: bool, create_iface_fails: bool) {
        let mut exec = fasync::TestExecutor::new();
        let mut test_values = test_setup();

        let (phy, mut phy_stream) = fake_phy();
        test_values.phys.insert(10, phy);

        let iface_counter = Arc::new(IfaceCounter::new());
        let inspector = Inspector::default();
        let mut ifaces_node = inspector.root().create_child("devices");

        // Initiate a CreateIface request. The returned future should not be able
        // to produce a result immediately
        let create_fut = super::create_iface(
            &test_values.new_iface_sink,
            &test_values.phys,
            fidl_svc::CreateIfaceRequest {
                phy_id: 10,
                role: fidl_wlan_common::WlanMacRole::Client,
                sta_addr: if with_mac { [0, 1, 2, 3, 4, 5] } else { NULL_ADDR.to_array() },
            },
            &iface_counter,
            &wlandevicemonitor_config::Config { wep_supported: true, wpa1_supported: true },
            &mut ifaces_node,
        );
        let mut create_fut = pin!(create_fut);
        assert_variant!(exec.run_until_stalled(&mut create_fut), Poll::Pending);

        // Validate the PHY request
        let bootstrap_channel = assert_variant!(exec.run_until_stalled(&mut phy_stream.next()),
            Poll::Ready(Some(Ok(fidl_dev::PhyRequest::CreateIface { req, responder }))) => {
                assert_eq!(fidl_wlan_common::WlanMacRole::Client, req.role);

                if with_mac {
                    assert_eq!(req.init_sta_addr, [0, 1, 2, 3, 4, 5]);
                }

                if create_iface_fails {
                    responder.send(Err(zx::sys::ZX_ERR_NOT_SUPPORTED)).expect("failed to send iface_id");
                } else {
                    responder.send(Ok(123)).expect("failed to send iface id");
                };

                req.mlme_channel
            }
        );

        // If this case should fail on interface creation, the future should complete here with an
        // error.
        if create_iface_fails {
            assert_variant!(
                exec.run_until_stalled(&mut create_fut),
                Poll::Ready(Err(zx::Status::NOT_SUPPORTED))
            );
            return;
        }

        if let Some(channel) = bootstrap_channel {
            let bootstrap_server =
                fidl::endpoints::ServerEnd::<fidl_sme::UsmeBootstrapMarker>::new(channel);
            let mut bootstrap_stream =
                bootstrap_server.into_stream().expect("Failed to make bootstrap stream");

            let responder = assert_variant!(
                exec.run_until_stalled(&mut bootstrap_stream.next()),
                Poll::Ready(Some(Ok(fidl_sme::UsmeBootstrapRequest::Start {
                    responder, ..
                }))) => responder);
            responder
                .send(inspector.duplicate_vmo().unwrap())
                .expect("Failed to send bootstrap result");
        }

        // The original future should resolve into a response.
        assert_variant!(exec.run_until_stalled(&mut create_fut),
            Poll::Ready(response) => {
                let response = response.expect("CreateIface failed unexpectedly");
                assert_eq!(0, response.id);
                assert_eq!(
                    device::PhyOwnership { phy_id: 10, phy_assigned_id: 123 },
                    response.phy_ownership
                );
            }
        );

        // The new interface should be pushed into the new iface stream.
        assert_variant!(
            exec.run_until_stalled(&mut test_values.new_iface_stream.next()),
            Poll::Ready(Some(NewIface { .. })),
        );
    }

    #[fuchsia::test]
    fn test_create_multiple_ifaces() {
        let mut exec = fasync::TestExecutor::new();
        let mut test_values = test_setup();
        let service_fut = serve_monitor_requests(
            test_values.monitor_req_stream,
            test_values.phys.clone(),
            test_values.ifaces,
            test_values.watcher_service,
            test_values.new_iface_sink,
            test_values.iface_counter,
            test_values.ifaces_node,
            fake_wlandevicemonitor_config(),
        );
        let mut service_fut = pin!(service_fut);
        assert_variant!(exec.run_until_stalled(&mut service_fut), Poll::Pending);

        // Request the list of available ifaces.
        let list_fut = test_values.monitor_proxy.list_ifaces();
        let mut list_fut = pin!(list_fut);
        assert_variant!(exec.run_until_stalled(&mut list_fut), Poll::Pending);

        // Progress the service loop.
        assert_variant!(exec.run_until_stalled(&mut service_fut), Poll::Pending);

        // The future to list the ifaces should complete and no ifaces should be present.
        assert_variant!(exec.run_until_stalled(&mut list_fut),Poll::Ready(Ok(ifaces)) => {
            assert!(ifaces.is_empty())
        });

        let (phy, mut phy_stream) = fake_phy();
        let phy_id = 10;
        test_values.phys.insert(phy_id, phy);

        for (sta_addr, phy_assigned_iface_id, sme_assigned_iface_id) in
            [([0x0, 0x1, 0x2, 0x3, 0x4, 0x5], 123, 0), ([0x6, 0x7, 0x8, 0x9, 0xa, 0xb], 0x123, 1)]
        {
            let create_iface_fut =
                test_values.monitor_proxy.create_iface(&fidl_svc::CreateIfaceRequest {
                    phy_id,
                    role: fidl_wlan_common::WlanMacRole::Client,
                    sta_addr,
                });
            let mut create_iface_fut = pin!(create_iface_fut);
            assert_variant!(exec.run_until_stalled(&mut create_iface_fut), Poll::Pending);
            assert_variant!(exec.run_until_stalled(&mut service_fut), Poll::Pending);

            // Validate the PHY request
            let bootstrap_channel = assert_variant!(exec.run_until_stalled(&mut phy_stream.next()),
            Poll::Ready(Some(Ok(fidl_dev::PhyRequest::CreateIface { req, responder }))) => {
                assert_eq!(fidl_wlan_common::WlanMacRole::Client, req.role);
                assert_eq!(req.init_sta_addr, sta_addr);
                responder.send(Ok(phy_assigned_iface_id)).expect("failed to send iface id");
                req.mlme_channel.expect("no MLME channel")
            });

            // Complete the USME bootstrap.
            let mut bootstrap_stream =
                fidl::endpoints::ServerEnd::<fidl_sme::UsmeBootstrapMarker>::new(bootstrap_channel)
                    .into_stream()
                    .expect("Failed to make bootstrap stream");
            assert_variant!(
                exec.run_until_stalled(&mut bootstrap_stream.next()),
                Poll::Ready(Some(Ok(fidl_sme::UsmeBootstrapRequest::Start {
                    responder, ..
                }))) => responder
                    .send(test_values.inspector.duplicate_vmo().unwrap())
                    .expect("Failed to send bootstrap result"));

            // The original future should resolve into a response.
            assert_variant!(exec.run_until_stalled(&mut service_fut), Poll::Pending);
            assert_variant!(exec.run_until_stalled(&mut create_iface_fut),
            Poll::Ready(Ok((zx::sys::ZX_OK, Some(response)))) => {
                assert_eq!(sme_assigned_iface_id, response.iface_id);
            });

            assert_variant!(
                exec.run_until_stalled(&mut test_values.new_iface_stream.next()),
                Poll::Ready(Some(NewIface { .. })),
            );
        }

        // Request the list of available ifaces.
        let list_fut = test_values.monitor_proxy.list_ifaces();
        let mut list_fut = pin!(list_fut);
        assert_variant!(exec.run_until_stalled(&mut list_fut), Poll::Pending);

        // Progress the service loop.
        assert_variant!(exec.run_until_stalled(&mut service_fut), Poll::Pending);

        // The future to list the ifaces should complete and the iface should be present.
        assert_variant!(exec.run_until_stalled(&mut list_fut), Poll::Ready(Ok(mut ifaces)) => {
            ifaces.sort();
            assert_eq!(vec![0, 1], ifaces);
        });
    }

    #[fuchsia::test]
    fn create_iface_on_invalid_phy_id() {
        let mut exec = fasync::TestExecutor::new();
        let test_values = test_setup();
        let iface_counter = Arc::new(IfaceCounter::new());
        let inspector = Inspector::default();
        let mut ifaces_node = inspector.root().create_child("devices");

        let fut = super::create_iface(
            &test_values.new_iface_sink,
            &test_values.phys,
            fidl_svc::CreateIfaceRequest {
                phy_id: 10,
                role: fidl_wlan_common::WlanMacRole::Client,
                sta_addr: NULL_ADDR.to_array(),
            },
            &iface_counter,
            &wlandevicemonitor_config::Config { wep_supported: true, wpa1_supported: true },
            &mut ifaces_node,
        );
        let mut fut = pin!(fut);
        assert_variant!(
            exec.run_until_stalled(&mut fut),
            Poll::Ready(Err(zx::Status::NOT_FOUND)),
            "expected failure on invalid PHY"
        );
    }

    fn fake_destroy_iface_env(
        phy_map: &PhyMap,
        iface_map: &IfaceMap,
    ) -> fidl_dev::PhyRequestStream {
        let (phy, phy_stream) = fake_phy();
        phy_map.insert(10, phy);
        // Create a generic SME proxy but drop the server since we won't use it.
        let (proxy, _) = create_proxy::<fidl_sme::GenericSmeMarker>()
            .expect("Failed to create generic SME proxy");
        iface_map.insert(
            42,
            device::IfaceDevice {
                phy_ownership: PhyOwnership { phy_id: 10, phy_assigned_id: 0 },
                generic_sme: proxy,
                inspect_node: None,
                inspect_vmo: None,
            },
        );
        phy_stream
    }

    #[fuchsia::test]
    fn destroy_iface_success() {
        let mut exec = fasync::TestExecutor::new();
        let test_values = test_setup();
        let mut phy_stream = fake_destroy_iface_env(&test_values.phys, &test_values.ifaces);

        let destroy_fut = super::destroy_iface(
            &test_values.phys,
            &test_values.ifaces,
            test_values.ifaces_node,
            42,
        );
        let mut destroy_fut = pin!(destroy_fut);
        assert_eq!(Poll::Pending, exec.run_until_stalled(&mut destroy_fut));

        let (req, responder) = assert_variant!(exec.run_until_stalled(&mut phy_stream.next()),
            Poll::Ready(Some(Ok(fidl_dev::PhyRequest::DestroyIface { req, responder }))) => (req, responder)
        );

        // Verify the destroy iface request to the corresponding PHY is correct.
        assert_eq!(0, req.id);

        responder.send(Ok(())).expect("failed to send DestroyIfaceResponse");
        assert_eq!(Poll::Ready(Ok(())), exec.run_until_stalled(&mut destroy_fut));

        // Verify iface was removed from available ifaces.
        assert!(test_values.ifaces.get(&42u16).is_none(), "iface expected to be deleted");
    }

    #[fuchsia::test]
    fn destroy_iface_failure() {
        let mut exec = fasync::TestExecutor::new();
        let test_values = test_setup();
        let mut phy_stream = fake_destroy_iface_env(&test_values.phys, &test_values.ifaces);

        let destroy_fut = super::destroy_iface(
            &test_values.phys,
            &test_values.ifaces,
            test_values.ifaces_node,
            42,
        );
        let mut destroy_fut = pin!(destroy_fut);
        assert_eq!(Poll::Pending, exec.run_until_stalled(&mut destroy_fut));

        let (req, responder) = assert_variant!(exec.run_until_stalled(&mut phy_stream.next()),
            Poll::Ready(Some(Ok(fidl_dev::PhyRequest::DestroyIface { req, responder }))) => (req, responder)
        );

        // Verify the destroy iface request to the corresponding PHY is correct.
        assert_eq!(0, req.id);

        responder.send(Err(zx::sys::ZX_ERR_INTERNAL)).expect("failed to send DestroyIfaceResponse");
        assert_eq!(
            Poll::Ready(Err(zx::Status::INTERNAL)),
            exec.run_until_stalled(&mut destroy_fut)
        );

        // Verify iface was not removed from available ifaces.
        assert!(test_values.ifaces.get(&42u16).is_some(), "iface expected to not be deleted");
    }

    #[fuchsia::test]
    fn destroy_iface_recovery() {
        let mut exec = fasync::TestExecutor::new();
        let test_values = test_setup();
        let mut phy_stream = fake_destroy_iface_env(&test_values.phys, &test_values.ifaces);

        let destroy_fut = super::destroy_iface(
            &test_values.phys,
            &test_values.ifaces,
            test_values.ifaces_node,
            42,
        );
        let mut destroy_fut = pin!(destroy_fut);
        assert_eq!(Poll::Pending, exec.run_until_stalled(&mut destroy_fut));

        let (req, responder) = assert_variant!(exec.run_until_stalled(&mut phy_stream.next()),
            Poll::Ready(Some(Ok(fidl_dev::PhyRequest::DestroyIface { req, responder }))) => (req, responder)
        );

        // Verify the destroy iface request to the corresponding PHY is correct.
        assert_eq!(0, req.id);

        // In the recovery scenario, the interface will have already been destroyed and the PHY
        // will have no record of it and will reply to the destruction request with a
        // ZX_ERR_NOT_FOUND.  In this case, we should verify that the internal accounting is still
        // updated.
        responder
            .send(Err(zx::sys::ZX_ERR_NOT_FOUND))
            .expect("failed to send DestroyIfaceResponse");
        assert_eq!(
            Poll::Ready(Err(zx::Status::NOT_FOUND)),
            exec.run_until_stalled(&mut destroy_fut)
        );

        // Verify iface was removed from available ifaces.
        assert!(test_values.ifaces.get(&42u16).is_none(), "iface should have been removed.");
    }

    #[fuchsia::test]
    fn destroy_iface_not_found() {
        let mut exec = fasync::TestExecutor::new();
        let test_values = test_setup();
        let _phy_stream = fake_destroy_iface_env(&test_values.phys, &test_values.ifaces);

        let fut = super::destroy_iface(
            &test_values.phys,
            &test_values.ifaces,
            test_values.ifaces_node,
            43,
        );
        let mut fut = pin!(fut);
        assert_eq!(Poll::Ready(Err(zx::Status::NOT_FOUND)), exec.run_until_stalled(&mut fut));
    }

    #[fuchsia::test]
    fn destroy_iface_phy_not_found() {
        let mut exec = fasync::TestExecutor::new();
        let test_values = test_setup();

        // Set the PHY ID of the interface to be some non-existent PHY ID.
        let (proxy, _) = create_proxy::<fidl_sme::GenericSmeMarker>()
            .expect("Failed to create generic SME proxy");
        test_values.ifaces.insert(
            1,
            device::IfaceDevice {
                phy_ownership: PhyOwnership { phy_id: 0, phy_assigned_id: 0 },
                generic_sme: proxy,
                inspect_node: None,
                inspect_vmo: None,
            },
        );

        // Destroy the interface that does not have a parent PHY ID.
        let fut = super::destroy_iface(
            &test_values.phys,
            &test_values.ifaces,
            test_values.ifaces_node,
            1,
        );
        let mut fut = pin!(fut);
        assert_eq!(Poll::Ready(Err(zx::Status::NOT_FOUND)), exec.run_until_stalled(&mut fut));

        assert!(test_values.ifaces.get(&1).is_none());
    }

    #[fuchsia::test]
    fn phy_removed_during_destroy_iface() {
        let mut exec = fasync::TestExecutor::new();
        let test_values = test_setup();
        let mut phy_stream = fake_destroy_iface_env(&test_values.phys, &test_values.ifaces);

        let destroy_fut = super::destroy_iface(
            &test_values.phys,
            &test_values.ifaces,
            test_values.ifaces_node,
            42,
        );
        let mut destroy_fut = pin!(destroy_fut);
        assert_eq!(Poll::Pending, exec.run_until_stalled(&mut destroy_fut));

        let (_req, responder) = assert_variant!(exec.run_until_stalled(&mut phy_stream.next()),
            Poll::Ready(Some(Ok(fidl_dev::PhyRequest::DestroyIface { req, responder }))) => (req, responder)
        );

        // Emulate the phy shutting down.
        drop(responder);
        drop(phy_stream);
        assert_eq!(Poll::Ready(Ok(())), exec.run_until_stalled(&mut destroy_fut));

        // Verify iface was removed from available ifaces despite the closure.
        assert!(test_values.ifaces.get(&42u16).is_none(), "iface expected to be deleted");
    }

    #[fuchsia::test]
    fn get_client_sme() {
        let mut exec = fasync::TestExecutor::new();
        let test_values = test_setup();
        let (phy, _phy_stream) = fake_phy();
        let phy_id = 10u16;
        test_values.phys.insert(phy_id, phy);
        let (generic_sme_proxy, mut generic_sme_stream) =
            create_proxy_and_stream::<fidl_sme::GenericSmeMarker>()
                .expect("Failed to create generic SME proxy and stream");

        test_values.ifaces.insert(
            42,
            device::IfaceDevice {
                phy_ownership: PhyOwnership { phy_id: 10, phy_assigned_id: 0 },
                generic_sme: generic_sme_proxy,
                inspect_node: None,
                inspect_vmo: None,
            },
        );

        let (client_sme_proxy, client_sme_server) =
            create_proxy::<fidl_sme::ClientSmeMarker>().expect("Failed to create client SME");

        let req_fut = super::get_client_sme(&test_values.ifaces, 42, client_sme_server);
        let mut req_fut = pin!(req_fut);
        assert_eq!(Poll::Pending, exec.run_until_stalled(&mut req_fut));

        // Respond to a client SME request with a client endpoint.
        let sme_server = assert_variant!(exec.run_until_stalled(&mut generic_sme_stream.next()),
            Poll::Ready(Some(Ok(fidl_sme::GenericSmeRequest::GetClientSme { sme_server, responder }))) => {
                responder.send(Ok(())).expect("Failed to send response");
                sme_server
            }
        );
        assert_variant!(exec.run_until_stalled(&mut req_fut), Poll::Ready(Ok(())));

        // Verify that the correct endpoint is served.
        let _status_req = client_sme_proxy.status();

        let mut sme_stream = sme_server.into_stream().expect("Failed to get client SME stream");
        assert_variant!(
            exec.run_until_stalled(&mut sme_stream.next()),
            Poll::Ready(Some(Ok(fidl_sme::ClientSmeRequest::Status { .. })))
        );
    }

    #[fuchsia::test]
    fn get_client_sme_fails() {
        let mut exec = fasync::TestExecutor::new();
        let test_values = test_setup();
        let (phy, _phy_stream) = fake_phy();
        let phy_id = 10u16;
        test_values.phys.insert(phy_id, phy);
        let (generic_sme_proxy, generic_sme_server) = create_proxy::<fidl_sme::GenericSmeMarker>()
            .expect("Failed to create generic SME proxy");

        test_values.ifaces.insert(
            42,
            device::IfaceDevice {
                phy_ownership: PhyOwnership { phy_id: 10, phy_assigned_id: 0 },
                generic_sme: generic_sme_proxy,
                inspect_node: None,
                inspect_vmo: None,
            },
        );

        let (_client_sme_proxy, client_sme_server) =
            create_proxy::<fidl_sme::ClientSmeMarker>().expect("Failed to create client SME");

        let req_fut = super::get_client_sme(&test_values.ifaces, 42, client_sme_server);
        let mut req_fut = pin!(req_fut);
        assert_eq!(Poll::Pending, exec.run_until_stalled(&mut req_fut));

        // Respond to a client SME request with an error.
        let mut generic_sme_stream =
            generic_sme_server.into_stream().expect("Failed to create generic SME stream");
        assert_variant!(exec.run_until_stalled(&mut generic_sme_stream.next()),
            Poll::Ready(Some(Ok(fidl_sme::GenericSmeRequest::GetClientSme { responder, .. }))) => {
                responder.send(Err(1)).expect("Failed to send response");
            }
        );
        assert_variant!(exec.run_until_stalled(&mut req_fut), Poll::Ready(Err(_)));
    }

    #[fuchsia::test]
    fn get_client_sme_invalid_iface() {
        let mut exec = fasync::TestExecutor::new();
        let test_values = test_setup();
        let (phy, _phy_stream) = fake_phy();
        let phy_id = 10u16;
        test_values.phys.insert(phy_id, phy);
        let (generic_sme_proxy, _generic_sme_server) = create_proxy::<fidl_sme::GenericSmeMarker>()
            .expect("Failed to create generic SME proxy");

        test_values.ifaces.insert(
            42,
            device::IfaceDevice {
                phy_ownership: PhyOwnership { phy_id: 10, phy_assigned_id: 0 },
                generic_sme: generic_sme_proxy,
                inspect_node: None,
                inspect_vmo: None,
            },
        );

        let (_client_sme_proxy, client_sme_server) =
            create_proxy::<fidl_sme::ClientSmeMarker>().expect("Failed to create client SME");

        let req_fut = super::get_client_sme(&test_values.ifaces, 1337, client_sme_server);
        let mut req_fut = pin!(req_fut);
        assert_variant!(exec.run_until_stalled(&mut req_fut), Poll::Ready(Err(_)));
    }

    #[fuchsia::test]
    fn get_feature_support() {
        let mut exec = fasync::TestExecutor::new();
        let test_values = test_setup();
        let (phy, _phy_stream) = fake_phy();
        let phy_id = 10u16;
        test_values.phys.insert(phy_id, phy);
        let (generic_sme_proxy, mut generic_sme_stream) =
            create_proxy_and_stream::<fidl_sme::GenericSmeMarker>()
                .expect("Failed to create generic SME proxy and stream");

        let (_feature_support_proxy, feature_support_server) =
            create_proxy::<fidl_sme::FeatureSupportMarker>().expect("Failed to create client SME");

        test_values.ifaces.insert(
            42,
            device::IfaceDevice {
                phy_ownership: PhyOwnership { phy_id: 10, phy_assigned_id: 0 },
                generic_sme: generic_sme_proxy,
                inspect_node: None,
                inspect_vmo: None,
            },
        );

        let req_fut = super::get_feature_support(&test_values.ifaces, 42, feature_support_server);
        let mut req_fut = pin!(req_fut);
        assert_eq!(Poll::Pending, exec.run_until_stalled(&mut req_fut));

        // Respond to a feature support request with a feature support endpoint.
        assert_variant!(exec.run_until_stalled(&mut generic_sme_stream.next()),
            Poll::Ready(Some(Ok(fidl_sme::GenericSmeRequest::GetFeatureSupport { responder, .. }))) => {
                responder.send(Ok(())).expect("Failed to send response");
            }
        );
        assert_variant!(exec.run_until_stalled(&mut req_fut), Poll::Ready(Ok(_)));
    }

    #[fuchsia::test]
    fn query_iface() {
        let mut exec = fasync::TestExecutor::new();
        let test_values = test_setup();
        let (phy, _phy_stream) = fake_phy();
        let phy_id = 10u16;
        test_values.phys.insert(phy_id, phy);
        let (generic_sme_proxy, mut generic_sme_stream) =
            create_proxy_and_stream::<fidl_sme::GenericSmeMarker>()
                .expect("Failed to create generic SME proxy and stream");

        test_values.ifaces.insert(
            42,
            device::IfaceDevice {
                phy_ownership: PhyOwnership { phy_id: 10, phy_assigned_id: 0 },
                generic_sme: generic_sme_proxy,
                inspect_node: None,
                inspect_vmo: None,
            },
        );

        let req_fut = super::query_iface(&test_values.ifaces, 42);
        let mut req_fut = pin!(req_fut);
        assert_eq!(Poll::Pending, exec.run_until_stalled(&mut req_fut));

        // Respond to a query with appropriate info.
        assert_variant!(
            exec.run_until_stalled(&mut generic_sme_stream.next()),
            Poll::Ready(Some(Ok(fidl_sme::GenericSmeRequest::Query { responder, .. }))) => {
                responder.send(&fidl_sme::GenericSmeQuery {
                    role: fidl_common::WlanMacRole::Client,
                    sta_addr: [2; 6],
                }).expect("Failed to send query response");
            }
        );
        let resp =
            assert_variant!(exec.run_until_stalled(&mut req_fut), Poll::Ready(Ok(resp)) => resp);
        assert_eq!(
            resp,
            fidl_svc::QueryIfaceResponse {
                role: fidl_fuchsia_wlan_common::WlanMacRole::Client,
                id: 42,
                phy_id: 10,
                phy_assigned_id: 0,
                sta_addr: [2; 6],
            }
        );
    }

    #[test_case(zx::Status::OK, false; "Generic SME with OK epitaph shuts down cleanly")]
    #[test_case(zx::Status::INTERNAL, true; "Generic SME with error epitaph initiates iface removal")]
    fn new_iface_stream_epitaph(epitaph: zx::Status, expect_destroy_iface: bool) {
        let mut exec = fasync::TestExecutor::new();
        let test_values = test_setup();
        let new_iface_fut = handle_new_iface_stream(
            test_values.phys.clone(),
            test_values.ifaces.clone(),
            test_values.ifaces_node,
            test_values.new_iface_stream,
        );
        let mut new_iface_fut = pin!(new_iface_fut);

        let (phy, mut phy_stream) = fake_phy();
        let phy_id = 10u16;
        test_values.phys.insert(phy_id, phy);

        let id = 42;
        let phy_ownership = PhyOwnership { phy_id, phy_assigned_id: 0 };

        let (generic_sme_proxy, generic_sme_server) =
            create_proxy::<fidl_sme::GenericSmeMarker>().expect("Failed to make generic SME");

        test_values.ifaces.insert(
            id,
            device::IfaceDevice {
                phy_ownership: phy_ownership.clone(),
                generic_sme: generic_sme_proxy.clone(),
                inspect_node: None,
                inspect_vmo: None,
            },
        );

        let new_iface = NewIface {
            id,
            phy_ownership,
            generic_sme: generic_sme_proxy,
            inspect_node: None,
            inspect_vmo: None,
        };
        test_values.new_iface_sink.unbounded_send(new_iface).expect("Failed to send new iface");
        assert_variant!(exec.run_until_stalled(&mut new_iface_fut), Poll::Pending);

        let (_generic_sme_stream, generic_sme_control) = generic_sme_server
            .into_stream_and_control_handle()
            .expect("Failed to get generic SME stream and control handle");
        generic_sme_control.shutdown_with_epitaph(epitaph);

        assert_variant!(exec.run_until_stalled(&mut new_iface_fut), Poll::Pending);

        if expect_destroy_iface {
            assert_variant!(
                exec.run_until_stalled(&mut phy_stream.next()),
                Poll::Ready(Some(Ok(fidl_dev::PhyRequest::DestroyIface { .. })))
            );
        } else {
            assert_variant!(exec.run_until_stalled(&mut phy_stream.next()), Poll::Pending);
        }
    }
}
