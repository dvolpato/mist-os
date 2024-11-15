// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub struct ResourceBinding {
    pub wire_path: String,
    pub optional_wire_path: String,
    pub natural_path: String,
}

pub struct ResourceBindings {
    pub handle: ResourceBinding,
}

impl Default for ResourceBindings {
    fn default() -> Self {
        Self {
            handle: ResourceBinding {
                wire_path: "::fidl_next::WireHandle".to_string(),
                optional_wire_path: "::fidl_next::WireOptionalHandle".to_string(),
                natural_path: "::fidl_next::Handle".to_string(),
            },
        }
    }
}
