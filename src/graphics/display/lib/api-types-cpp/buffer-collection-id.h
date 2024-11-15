// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_BUFFER_COLLECTION_ID_H_
#define SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_BUFFER_COLLECTION_ID_H_

#include <fidl/fuchsia.hardware.display/cpp/wire.h>

#include <cstdint>

#include <fbl/strong_int.h>

namespace display {

// More useful representation of `fuchsia.hardware.display/BufferCollectionId`.
//
// See `DriverBufferCollectionId` for the type used at the interface between the
// display coordinator and the display drivers.
DEFINE_STRONG_INT(BufferCollectionId, uint64_t);

constexpr BufferCollectionId ToBufferCollectionId(
    fuchsia_hardware_display::wire::BufferCollectionId fidl_buffer_collection_id) {
  return BufferCollectionId(fidl_buffer_collection_id.value);
}
constexpr fuchsia_hardware_display::wire::BufferCollectionId ToFidlBufferCollectionId(
    BufferCollectionId buffer_collection_id) {
  return {.value = buffer_collection_id.value()};
}
}  // namespace display

#endif  // SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_BUFFER_COLLECTION_ID_H_
