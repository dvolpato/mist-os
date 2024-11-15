// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_SYSMEM_SERVER_USAGE_PIXEL_FORMAT_COST_H_
#define SRC_SYSMEM_SERVER_USAGE_PIXEL_FORMAT_COST_H_

#include <fidl/fuchsia.sysmem2/cpp/fidl.h>
#include <fidl/fuchsia.sysmem2/cpp/wire.h>

#include <cstdint>

namespace sysmem_service {

// This class effectively breaks ties in a platform-specific way among the list
// of PixelFormat(s) that a set of participants are all able to support.
//
// At first, the list of PixelFormat(s) that all participants are able to
// support is likely to be a short list.  But even if that list is only 2
// entries long, we'll typically want to prefer a particular choice depending
// on considerations like max throughput, power usage, efficiency
// considerations, etc.
//
// For now, the overrides are baked into sysmem based on the platform ID (AKA
// PID), in usage_overrides_*.cpp.
//
// Any override will take precedence over the default PixelFormat sort order.
class UsagePixelFormatCost {
 public:
  UsagePixelFormatCost(std::vector<fuchsia_sysmem2::FormatCostEntry> entries);
  // Compare the cost of two pixel formats, returning -1 if the first format
  // is lower cost, 0 if they're equal cost or unknown, and 1 if the first
  // format is higher cost.
  //
  // By passing in the BufferCollectionConstraints, the implementation can
  // consider other aspects of constraints in addition to the usage.
  int32_t Compare(const fuchsia_sysmem2::BufferCollectionConstraints& constraints,
                  uint32_t image_format_constraints_index_a,
                  uint32_t image_format_constraints_index_b) const;

 private:
  double GetCost(const fuchsia_sysmem2::BufferCollectionConstraints& constraints,
                 uint32_t image_format_constraints_index) const;
  const std::vector<fuchsia_sysmem2::FormatCostEntry> entries_;
};

}  // namespace sysmem_service

#endif  // SRC_SYSMEM_SERVER_USAGE_PIXEL_FORMAT_COST_H_
