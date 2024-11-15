// Copyright 2024 Mist Tecnologia LTDA
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/vfs/mount.h"

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/task/kernel.h>
#include <lib/mistos/starnix/kernel/vfs/dir_entry.h>
#include <lib/mistos/starnix/kernel/vfs/file_system.h>
#include <lib/mistos/starnix/kernel/vfs/fs_node.h>
#include <lib/mistos/starnix/kernel/vfs/mount_info.h>
#include <lib/mistos/starnix_uapi/mount_flags.h>
#include <lib/mistos/starnix_uapi/open_flags.h>
#include <trace.h>

#include <fbl/ref_ptr.h>
#include <ktl/optional.h>

#include "../kernel_priv.h"

#include <ktl/enforce.h>

#define LOCAL_TRACE STARNIX_KERNEL_GLOBAL_TRACE(0)

using namespace starnix_uapi;

namespace starnix {

MountInfo::~MountInfo() = default;

MountInfo MountInfo::detached() { return {ktl::nullopt}; }

MountFlags MountInfo::flags() {
  if (handle_.has_value()) {
    return handle_.value()->flags();
  } else {
    // Consider not mounted node have the NOATIME flags.
    return MountFlags(MountFlagsEnum::NOATIME);
  }
}

fit::result<Errno> MountInfo::check_readonly_filesystem() {
  if (flags().contains(MountFlagsEnum::RDONLY)) {
    return fit::error(errno(EROFS));
  }
  return fit::ok();
}

ktl::optional<MountHandle> MountInfo::operator*() const { return handle_; }

NamespaceNode Mount::root() {
  return NamespaceNode{.mount_ = MountInfo{.handle_ = fbl::RefPtr<Mount>(this)}, .entry_ = root_};
}

ktl::optional<NamespaceNode> Mount::mountpoint() const {
  auto state = state_.Read();
  auto &[mount, entry] = state->mountpoint.value();
  return NamespaceNode::New(mount.Lock(), entry);
}

MountFlags Mount::flags() const { return *flags_.Lock(); }

MountHandle Mount::New(WhatToMount what, MountFlags flags) {
  switch (what.type) {
    case WhatToMountEnum::Fs: {
      auto fs = ktl::get<FileSystemHandle>(what.what);
      return new_with_root(fs->root(), flags);
    }
    case WhatToMountEnum::Bind:
      return MountHandle();
  }
}

MountHandle Mount::new_with_root(DirEntryHandle root, MountFlags flags) {
  auto known_flags = MountFlags(MountFlagsEnum::STORED_ON_MOUNT);
  ASSERT(!flags.intersects(known_flags));

  auto fs = root->node_->fs();
  auto kernel = fs->kernel_.Lock();
  ASSERT_MSG(kernel, "can't create mount without a kernel");

  fbl::AllocChecker ac;
  auto handle = fbl::AdoptRef(new (&ac) Mount(kernel->next_mount_id_.next(), flags, root, fs));
  ZX_ASSERT(ac.check());
  return handle;
}

Mount::Mount(uint64_t id, MountFlags flags, DirEntryHandle root, FileSystemHandle fs)
    : root_(ktl::move(root)), fs_(ktl::move(fs)), flags_(ktl::move(flags)), id_(id) {}

Mount::~Mount() = default;

}  // namespace starnix
