// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context, Result};
use fuchsia_async::{self as fasync, DurationExt, Timer};
use fuchsia_component::client;
use fuchsia_component_test::RealmBuilder;
use fuchsia_driver_test::{DriverTestRealmBuilder, DriverTestRealmInstance};
use {fidl_fuchsia_driver_test as fdt, fidl_fuchsia_services_test as ft};

#[fasync::run_singlethreaded(test)]
async fn test_services() -> Result<()> {
    // Create the RealmBuilder.
    let builder = RealmBuilder::new().await?;
    builder.driver_test_realm_setup().await?;

    let expose = fuchsia_component_test::Capability::service::<ft::DeviceMarker>().into();
    let dtr_exposes = vec![expose];

    builder.driver_test_realm_add_dtr_exposes(&dtr_exposes).await?;
    // Build the Realm.
    let realm = builder.build().await?;

    // Start the DriverTestRealm.
    let args = fdt::RealmArgs {
        root_driver: Some("#meta/root.cm".to_string()),
        dtr_exposes: Some(dtr_exposes),
        ..Default::default()
    };
    realm.driver_test_realm_start(args).await?;

    // Find an instance of the `Device` service.
    let instance;
    let service = client::open_service_at_dir::<ft::DeviceMarker>(realm.root.get_exposed_dir())
        .context("Failed to open service")?;
    loop {
        // TODO(https://fxbug.dev/42124541): Once component manager supports watching for
        // service instances, this loop shousld be replaced by a watcher.
        let entries = fuchsia_fs::directory::readdir(&service)
            .await
            .context("Failed to read service instances")?;
        if let Some(entry) = entries.iter().next() {
            instance = entry.name.clone();
            break;
        }
        Timer::new(zx::MonotonicDuration::from_millis(100).after_now()).await;
    }

    // Connect to the `Device` service.
    let device = client::connect_to_service_instance_at_dir::<ft::DeviceMarker>(
        realm.root.get_exposed_dir(),
        &instance,
    )
    .context("Failed to open service")?;
    // Use the `ControlPlane` protocol from the `Device` service.
    let control = device.connect_to_control()?;
    control.control_do().await?;
    // Use the `DataPlane` protocol from the `Device` service.
    let data = device.connect_to_data()?;
    data.data_do().await?;

    Ok(())
}
