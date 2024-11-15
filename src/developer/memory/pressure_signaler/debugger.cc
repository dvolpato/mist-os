// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/memory/pressure_signaler/debugger.h"

#include <lib/syslog/cpp/macros.h>

namespace pressure_signaler {

MemoryDebugger::MemoryDebugger(sys::ComponentContext* context, PressureNotifier* notifier)
    : notifier_(notifier) {
  FX_CHECK(notifier_);
  FX_CHECK(context);
  zx_status_t status = context->outgoing()->AddPublicService(bindings_.GetHandler(this));
  FX_CHECK(status == ZX_OK);
}

void MemoryDebugger::Signal(fuchsia::memorypressure::Level level) { notifier_->DebugNotify(level); }

}  // namespace pressure_signaler
