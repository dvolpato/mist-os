// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_TESTING_TESTING_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_TESTING_TESTING_H_

#include <lib/mistos/starnix/kernel/task/current_task.h>
#include <lib/mistos/starnix/kernel/vfs/file_system_ops.h>
#include <lib/mistos/starnix/kernel/vfs/fs_node_ops.h>

#include <fbl/ref_ptr.h>
#include <ktl/optional.h>
#include <ktl/pair.h>
#include <ktl/string_view.h>

namespace starnix {

class Kernel;

namespace testing {

class AutoReleasableTask {
 public:
  static AutoReleasableTask From(starnix::TaskBuilder builder) {
    return AutoReleasableTask::From(starnix::CurrentTask::From(ktl::move(builder)));
  }

  static AutoReleasableTask From(starnix::CurrentTask task) {
    return AutoReleasableTask({ktl::move(task)});
  }

  starnix::CurrentTask& operator*() {
    ASSERT_MSG(task_.has_value(),
               "called `operator*` on ktl::optional that does not contain a value.");
    return task_.value();
  }

  const starnix::CurrentTask& operator*() const {
    ASSERT_MSG(task_.has_value(),
               "called `operator*` on ktl::optional that does not contain a value.");
    return task_.value();
  }

  starnix::CurrentTask* operator->() {
    ASSERT_MSG(task_.has_value(),
               "called `operator->` on ktl::optional that does not contain a value.");
    return &task_.value();
  }

  const starnix::CurrentTask* operator->() const {
    ASSERT_MSG(task_.has_value(),
               "called `operator->` on ktl::optional that does not contain a value.");
    return &task_.value();
  }

  ~AutoReleasableTask() { task_->release(); }

  AutoReleasableTask(AutoReleasableTask&& other) noexcept {
    task_ = ktl::move(other.task_);
    other.task_ = ktl::nullopt;
  }

  AutoReleasableTask& operator=(AutoReleasableTask&& other) noexcept {
    task_ = ktl::move(other.task_);
    other.task_ = ktl::nullopt;
    return *this;
  }

 private:
  explicit AutoReleasableTask(ktl::optional<starnix::CurrentTask> task) : task_(ktl::move(task)) {}

  DISALLOW_COPY_AND_ASSIGN_ALLOW_MOVE(AutoReleasableTask);

  ktl::optional<starnix::CurrentTask> task_ = ktl::nullopt;
};

/// An old way of creating a task for testing
///
/// This way of creating a task has problems because the test isn't actually run with that task
/// being current, which means that functions that expect a CurrentTask to actually be mapped into
/// memory can operate incorrectly.
///
/// Please use `spawn_kernel_and_run` instead. If there isn't a variant of `spawn_kernel_and_run`
/// for this use case, please consider adding one that follows the new pattern of actually running
/// the test on the spawned task.
ktl::pair<fbl::RefPtr<Kernel>, starnix::testing::AutoReleasableTask>
create_kernel_task_and_unlocked_with_bootfs();

ktl::pair<fbl::RefPtr<Kernel>, starnix::testing::AutoReleasableTask>
create_kernel_task_and_unlocked_with_bootfs_current_zbi();

/// An old way of creating a task for testing
///
/// This way of creating a task has problems because the test isn't actually run with that task
/// being current, which means that functions that expect a CurrentTask to actually be mapped into
/// memory can operate incorrectly.
///
/// Please use `spawn_kernel_and_run` instead. If there isn't a variant of `spawn_kernel_and_run`
/// for this use case, please consider adding one that follows the new pattern of actually running
/// the test on the spawned task.
ktl::pair<fbl::RefPtr<starnix::Kernel>, AutoReleasableTask> create_kernel_and_task();

/// An old way of creating a task for testing
///
/// This way of creating a task has problems because the test isn't actually run with that task
/// being current, which means that functions that expect a CurrentTask to actually be mapped into
/// memory can operate incorrectly.
///
/// Please use `spawn_kernel_and_run` instead. If there isn't a variant of `spawn_kernel_and_run`
/// for this use case, please consider adding one that follows the new pattern of actually running
/// the test on the spawned task.
ktl::pair<fbl::RefPtr<Kernel>, starnix::testing::AutoReleasableTask>
    create_kernel_and_task_with_selinux(/*security_server: Arc<SecurityServer>*/);

/// Create a Kernel object and run the given callback in the init process for that kernel.
///
/// This function is useful if you want to test code that requires a CurrentTask because
/// your callback is called with the init process as the CurrentTask.
// pub fn spawn_kernel_and_run<F>(callback: F)

/// An old way of creating a task for testing
///
/// This way of creating a task has problems because the test isn't actually run with that task
/// being current, which means that functions that expect a CurrentTask to actually be mapped into
/// memory can operate incorrectly.
///
/// Please use `spawn_kernel_and_run` instead. If there isn't a variant of `spawn_kernel_and_run`
/// for this use case, please consider adding one that follows the new pattern of actually running
/// the test on the spawned task.
ktl::pair<fbl::RefPtr<starnix::Kernel>, AutoReleasableTask> create_kernel_task_and_unlocked();

/// An old way of creating a task for testing
///
/// This way of creating a task has problems because the test isn't actually run with that task
/// being current, which means that functions that expect a CurrentTask to actually be mapped into
/// memory can operate incorrectly.
///
/// Please use `spawn_kernel_and_run` instead. If there isn't a variant of `spawn_kernel_and_run`
/// for this use case, please consider adding one that follows the new pattern of actually running
/// the test on the spawned task.
ktl::pair<fbl::RefPtr<Kernel>, starnix::testing::AutoReleasableTask>
    create_kernel_task_and_unlocked_with_selinux(/*security_server: Arc<SecurityServer>*/);

fbl::RefPtr<Kernel> create_test_kernel(/*security_server: Arc<SecurityServer>*/);

TaskBuilder create_test_init_task(fbl::RefPtr<Kernel> kernel, fbl::RefPtr<FsContext> fs);

/// An old way of creating a task for testing
///
/// This way of creating a task has problems because the test isn't actually run with that task
/// being current, which means that functions that expect a CurrentTask to actually be mapped
/// into memory can operate incorrectly.
///
/// Please use `spawn_kernel_and_run` instead. If there isn't a variant of
/// `spawn_kernel_and_run` for this use case, please consider adding one that follows the new
/// pattern of actually running the test on the spawned task.
AutoReleasableTask create_task(fbl::RefPtr<starnix::Kernel>& kernel,
                               const ktl::string_view& task_name);

// Maps `length` at `address` with `PROT_READ | PROT_WRITE`, `MAP_ANONYMOUS | MAP_PRIVATE`.
//
// Returns the address returned by `sys_mmap`.
UserAddress map_memory(starnix::CurrentTask& current_task, UserAddress address, uint64_t length);

// Maps `length` at `address` with `PROT_READ | PROT_WRITE` and the specified flags.
//
// Returns the address returned by `sys_mmap`.
UserAddress map_memory_with_flags(starnix::CurrentTask& current_task, UserAddress address,
                                  uint64_t length, uint32_t flags);

class TestFs : public FileSystemOps {
 public:
  fit::result<Errno, struct statfs> statfs(const FileSystem& fs,
                                           const CurrentTask& current_task) override {
    return fit::ok(default_statfs(0));
  }

  const FsStr& name() override { return name_; }

  bool generate_node_ids() override { return false; }

 private:
  const FsStr name_ = "test";
};

FileSystemHandle create_fs(fbl::RefPtr<starnix::Kernel>& kernel, FsNodeOps* ops);

}  // namespace testing
}  // namespace starnix

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_TESTING_TESTING_H_
