// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// TODO(https://fxbug.dev/42055130): ffx auto-imports these libraries, even though we don't need them.
use {argh as _, ffx_core as _};

pub use package_tool::PackageArchiveRemoveCommand;

pub type FfxPluginCommand = PackageArchiveRemoveCommand;
