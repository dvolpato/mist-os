// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <stddef.h>

#include "src/storage/lib/ftl/ftl.h"
#include "src/storage/lib/ftl/ftl_private.h"
#include "src/storage/lib/ftl/ndm/ndmp.h"

// Global Function Definitions

//   ndmDelVol: Un-initialize a Blunk file system volume, or custom
//              one, for a partition entry in the partition table
//
//      Inputs: ndm = pointer to NDM control block
//              part_num = partition number
//
//     Returns: 0 on success, -1 on error
//
int ndmDelVol(NDM ndm, ui32 part_num) {
  const NDMPartition* part;

  // Get handle to entry in partitions table. Return if error.
  part = ndmGetPartition(ndm, part_num);
  if (part == NULL)
    return -1;

  // Remove partition's FTL volume. Return status.
  return FtlNdmDelVol(&ndm->vols, part->name);
}

//  ndmDelVols: Loop through partition table un-initializing valid
//              partitions
//
//       Input: ndm = pointer to NDM control block
//
//     Returns: 0 on success, -1 on failure
//
int ndmDelVols(NDM ndm) {
  ui32 i, num_partitions;
  int status = 0;

  // Get the total number of partitions.
  num_partitions = ndmGetNumPartitions(ndm);

  // Loop through all partitions, un-initializing valid ones.
  for (i = 0; i < num_partitions; ++i)
    if (ndmDelVol(ndm, i))
      status = -1;

  // Return status.
  return status;
}
