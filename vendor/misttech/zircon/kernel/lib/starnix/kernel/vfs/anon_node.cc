// Copyright 2024 Mist Tecnlogia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/vfs/anon_node.h"

#include <lib/mistos/starnix/kernel/task/current_task.h>
#include <lib/mistos/starnix/kernel/task/kernel.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix/kernel/vfs/file_object.h>
#include <lib/mistos/starnix/kernel/vfs/file_ops.h>
#include <zircon/assert.h>

// #include <ktl/enforce.h>

namespace starnix {

FileHandle Anon::new_file_extended(const CurrentTask& current_task, ktl::unique_ptr<FileOps> ops,
                                   OpenFlags flags, std::function<FsNodeInfo(ino_t)> info) {
  fbl::AllocChecker ac;
  auto anon = new (&ac) Anon();
  ZX_ASSERT(ac.check());

  auto fs = anon_fs(current_task->kernel());
  return FileObject::new_anonymous(
      ktl::move(ops), fs->create_node(current_task, ktl::unique_ptr<FsNodeOps>(anon), info), flags);
}

FileHandle Anon::new_file(const CurrentTask& current_task, ktl::unique_ptr<FileOps> ops,
                          OpenFlags flags) {
  return new_file_extended(
      current_task, ktl::move(ops), flags,
      FsNodeInfo::new_factory(FileMode::from_bits(0600), current_task->as_fscred()));
}

FileSystemHandle anon_fs(const fbl::RefPtr<Kernel>& kernel) {
  if (!kernel->anon_fs_.is_initialized()) {
    fbl::AllocChecker ac;
    auto anonfs = new (&ac) AnonFs();
    ZX_ASSERT(ac.check());
    kernel->anon_fs_.set(FileSystem::New(kernel, {.type = CacheModeType::Uncached}, anonfs, {}));
  }
  return kernel->anon_fs_.get();
}

}  // namespace starnix
