// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_DIR_ENTRY_H_
#define ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_DIR_ENTRY_H_

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/vfs/fs_node.h>
#include <lib/mistos/starnix/kernel/vfs/mount_info.h>
#include <lib/mistos/starnix/kernel/vfs/path.h>
#include <lib/mistos/starnix_uapi/errors.h>
#include <lib/mistos/util/error_propagation.h>
#include <lib/mistos/util/weak_wrapper.h>
#include <lib/starnix_sync/locks.h>

#include <utility>

#include <fbl/ref_counted_upgradeable.h>
#include <kernel/mutex.h>
#include <ktl/optional.h>
#include <ktl/unique_ptr.h>
#include <ktl/variant.h>

namespace unit_testing {
bool test_tmpfs();
}

namespace starnix {

using namespace starnix_sync;

class CurrentTask;
class DirEntry;
class FsNode;

using DirEntryHandle = fbl::RefPtr<DirEntry>;
using FsNodeHandle = fbl::RefPtr<FsNode>;

struct DirEntryState {
  /// The parent DirEntry.
  ///
  /// The DirEntry tree has strong references from child-to-parent and weak
  /// references from parent-to-child. This design ensures that the parent
  /// chain is always populated in the cache, but some children might be
  /// missing from the cache.
  ktl::optional<DirEntryHandle> parent;

  /// The name that this parent calls this child.
  ///
  /// This name might not be reflected in the full path in the namespace that
  /// contains this DirEntry. For example, this DirEntry might be the root of
  /// a chroot.
  ///
  /// Most callers that want to work with names for DirEntries should use the
  /// NamespaceNodes.
  FsString local_name;

  /// Whether this directory entry has been removed from the tree.
  bool is_dead;

  /// The number of filesystem mounted on the directory entry.
  uint32_t mount_count;
};

class DirEntryOps {
 public:
  /// Revalidate the [`DirEntry`], if needed.
  ///
  /// Most filesystems don't need to do any revalidations because they are "local"
  /// and all changes to nodes go through the kernel. However some filesystems
  /// allow changes to happen through other means (e.g. NFS, FUSE) and these
  /// filesystems need a way to let the kernel know it may need to refresh its
  /// cached metadata. This method provides that hook for such filesystems.
  ///
  /// For more details, see:
  ///  - https://www.halolinux.us/kernel-reference/the-dentry-cache.html
  ///  -
  ///  https://www.kernel.org/doc/html/latest/filesystems/path-lookup.html#revalidation-and-automounts
  ///  - https://lwn.net/Articles/649115/
  ///  - https://www.infradead.org/~mchehab/kernel_docs/filesystems/path-walking.html
  ///
  /// Returns `Ok(valid)` where `valid` indicates if the `DirEntry` is still valid,
  /// or an error.
  virtual fit::result<Errno, bool> revalidate(const CurrentTask& current_task, const DirEntry&);

  virtual ~DirEntryOps();
};

class DefaultDirEntryOps : public DirEntryOps {
 public:
  virtual ~DefaultDirEntryOps();
};

struct Created {};

template <typename CreateFn>
struct Existed {
 public:
  Existed(CreateFn&& fn) : create_fn(std::forward<CreateFn>(fn)) {}

  CreateFn create_fn;
};

template <typename CreateFn>
struct CreationResult {
 public:
  CreationResult(const Created& c) : variant_(c) {}
  CreationResult(CreateFn&& fn) : variant_(Existed<CreateFn>(std::forward<CreateFn>(fn))) {}

  ktl::variant<Created, Existed<CreateFn>> variant_;
};

// Helpers from the reference documentation for std::visit<>, to allow
// visit-by-overload of the std::variant<>
template <class... Ts>
struct overloaded : Ts... {
  using Ts::operator()...;
};

// explicit deduction guide (not needed as of C++20)
template <class... Ts>
overloaded(Ts...) -> overloaded<Ts...>;

/// An entry in a directory.
///
/// This structure assigns a name to an FsNode in a given file system. An
/// FsNode might have multiple directory entries, for example if there are more
/// than one hard link to the same FsNode. In those cases, each hard link will
/// have a different parent and a different local_name because each hard link
/// has its own DirEntry object.
///
/// A directory cannot have more than one hard link, which means there is a
/// single DirEntry for each Directory FsNode. That invariant lets us store the
/// children for a directory in the DirEntry rather than in the FsNode.
class DirEntry
    : public fbl::WAVLTreeContainable<util::WeakPtr<DirEntry>, fbl::NodeOptions::AllowClearUnsafe>,
      private fbl::RefCountedUpgradeable<DirEntry> {
 public:
  using DirEntryChildren = fbl::WAVLTree<FsString, util::WeakPtr<DirEntry>>;

  /// The FsNode referenced by this DirEntry.
  ///
  /// A given FsNode can be referenced by multiple DirEntry objects, for
  /// example if there are multiple hard links to a given FsNode.
  FsNodeHandle node_;

  /// The [`DirEntryOps`] for this `DirEntry`.
  ///
  /// The `DirEntryOps` are implemented by the individual file systems to provide
  /// specific behaviours for this `DirEntry`.
  ktl::unique_ptr<DirEntryOps> ops_;

  /// The mutable state for this DirEntry.
  ///
  /// Leaf lock - do not acquire other locks while holding this one.
  mutable RwLock<DirEntryState> state_;

  /// A partial cache of the children of this DirEntry.
  ///
  /// DirEntries are added to this cache when they are looked up and removed
  /// when they are no longer referenced.
  ///
  /// This is separated from the DirEntryState for lock ordering. rename needs to lock the source
  /// parent, the target parent, the source, and the target - four (4) DirEntries in total.
  /// Getting the ordering right on these is nearly impossible. However, we only need to lock the
  /// children map on the two parents and we don't need to lock the children map on the two
  /// children. So splitting the children out into its own lock resolves this.
  mutable RwLock<DirEntryChildren> children_;

  /// impl DirEntry
  static DirEntryHandle New(FsNodeHandle node, ktl::optional<DirEntryHandle> parent,
                            FsString local_name);

  /// Returns a new DirEntry for the given `node` without parent. The entry has no local name.
  static DirEntryHandle new_unrooted(FsNodeHandle node);

  class DirEntryLockedChildren {
   private:
    DirEntryHandle entry_;

    RwLock<DirEntryChildren>::RwLockWriteGuard children_;

   public:
    DirEntryLockedChildren(DirEntryHandle entry,
                           RwLock<DirEntry::DirEntryChildren>::RwLockWriteGuard children)
        : entry_(ktl::move(entry)), children_(ktl::move(children)) {}

    /// impl<'a> DirEntryLockedChildren<'a>
    template <typename CreateNodeFn>
    fit::result<Errno, ktl::pair<DirEntryHandle, CreationResult<CreateNodeFn>>> get_or_create_child(
        const CurrentTask& current_task, const MountInfo& mount, const FsStr& name,
        CreateNodeFn&& create_fn) {
      static_assert(std::is_invocable_r_v<fit::result<Errno, FsNodeHandle>, CreateNodeFn,
                                          const FsNodeHandle&, const MountInfo&, const FsStr&>);
      auto create_child = [&](CreateNodeFn&& create_fn)
          -> fit::result<Errno, ktl::pair<DirEntryHandle, CreationResult<CreateNodeFn>>> {
        auto find_or_create_node = [&](CreateNodeFn&& create_fn)
            -> fit::result<Errno, ktl::pair<FsNodeHandle, CreationResult<CreateNodeFn>>> {
          // Before creating the child, check for existence.
          auto lookup_result = entry_->node_->lookup(current_task, mount, name);
          if (lookup_result.is_ok()) {
            return fit::ok(
                ktl::pair(lookup_result.value(), CreationResult<CreateNodeFn>(create_fn)));
          } else {
            if (lookup_result.error_value().error_code() == ENOENT) {
              if (auto create_fn_result = create_fn(entry_->node_, mount, name);
                  create_fn_result.is_error()) {
                return create_fn_result.take_error();
              } else {
                return fit::ok(ktl::pair(create_fn_result.value(), Created()));
              }
            } else {
              return lookup_result.take_error();
            }
          }
        }(create_fn);

        if (find_or_create_node.is_error())
          return find_or_create_node.take_error();

        auto [node, create_result] = find_or_create_node.value();

        ASSERT_MSG((node->info()->mode & FileMode::IFMT) != FileMode::EMPTY,
                   "FsNode initialization did not populate the FileMode in FsNodeInfo.");

        auto entry = DirEntry::New(node, {entry_}, name);

        // #[cfg(any(test, debug_assertions))]
        {
          // Take the lock on child while holding the one on the parent to ensure any wrong
          // ordering will trigger the tracing-mutex at the right call site.
          // auto _l1 = entry->state().Read();
        }

        return fit::ok(ktl::pair(entry, create_result));
      };

      auto result =
          [&]() -> fit::result<Errno, ktl::pair<DirEntryHandle, CreationResult<CreateNodeFn>>> {
        if (auto it = children_->find(name); it == children_->end()) {
          // Vacant
          if (auto result = create_child(create_fn); result.is_error()) {
            return result.take_error();
          } else {
            auto [child, create_result] = result.value();
            children_->insert(util::WeakPtr(child.get()));
            return fit::ok(ktl::pair(child, create_result));
          }
        } else {
          // Occupied
          // It's possible that the upgrade will succeed this time around because we dropped
          // the read lock before acquiring the write lock. Another thread might have
          // populated this entry while we were not holding any locks.
          auto child = it.CopyPointer().Lock();
          if (child) {
            child->node_->fs()->did_access_dir_entry(child);
            return fit::ok(ktl::pair(child, CreationResult<CreateNodeFn>(create_fn)));
          }

          if (auto result = create_child(create_fn); result.is_error()) {
            return result.take_error();
          } else {
            auto [new_child, create_result] = result.value();
            children_->insert(util::WeakPtr(new_child.get()));
            return fit::ok(ktl::pair(new_child, create_result));
          }
        }
      }();

      if (result.is_error()) {
        return result.take_error();
      }

      auto [child, create_result] = result.value();
      child->node_->fs()->did_create_dir_entry(child);
      return fit::ok(ktl::pair(child, create_result));
    }
  };

 private:
  DirEntryLockedChildren lock_children();

 public:
  /// The name that this node's parent calls this node.
  ///
  /// If this node is mounted in a namespace, the parent of this node in that
  /// namespace might have a different name for the point in the namespace at
  /// which this node is mounted.
  FsString local_name() const;

  /// The parent DirEntry object or this DirEntry if this entry is the root.
  ///
  /// Useful when traversing up the tree if you always want to find a parent
  /// (e.g., for "..").
  ///
  /// Be aware that the root of one file system might be mounted as a child
  /// in another file system. For that reason, consider walking the
  /// NamespaceNode tree (which understands mounts) rather than the DirEntry
  /// tree.
  DirEntryHandle parent_or_self();

  /// Whether this directory entry has been removed from the tree.
  bool is_dead() const { return state_.Read()->is_dead; }

  /// Whether the given name has special semantics as a directory entry.
  ///
  /// Specifically, whether the name is empty (which means "self"), dot
  /// (which also means "self"), or dot dot (which means "parent").
  static bool is_reserved_name(const FsStr& name) {
    return name.empty() || name == "." || name == "..";
  }

  /// Look up a directory entry with the given name as direct child of this
  /// entry.
  fit::result<Errno, DirEntryHandle> component_lookup(const CurrentTask& current_task,
                                                      const MountInfo& mount, const FsStr& name);

  /// Creates a new DirEntry
  ///
  /// The create_node_fn function is called to create the underlying FsNode
  /// for the DirEntry.
  ///
  /// If the entry already exists, create_node_fn is not called, and EEXIST is
  /// returned.
  template <typename CreateNodeFn>
  fit::result<Errno, DirEntryHandle> create_entry(const CurrentTask& current_task,
                                                  const MountInfo& mount, const FsStr& name,
                                                  CreateNodeFn&& fn) {
    static_assert(std::is_invocable_r_v<fit::result<Errno, FsNodeHandle>, CreateNodeFn,
                                        const FsNodeHandle&, const MountInfo&, const FsStr&>);

    auto result = create_entry_internal(current_task, mount, name, fn);
    if (result.is_error()) {
      return result.take_error();
    }

    auto [entry, exists] = result.value();
    if (exists) {
      return fit::error(errno(EEXIST));
    }
    return fit::ok(entry);
  }

  /// Creates a new DirEntry. Works just like create_entry, except if the entry already exists,
  /// it is returned.
  template <typename CreateNodeFn>
  fit::result<Errno, DirEntryHandle> get_or_create_entry(const CurrentTask& current_task,
                                                         const MountInfo& mount, const FsStr& name,
                                                         CreateNodeFn&& fn) {
    static_assert(std::is_invocable_r_v<fit::result<Errno, FsNodeHandle>, CreateNodeFn,
                                        const FsNodeHandle&, const MountInfo&, const FsStr&>);

    auto result = create_entry_internal(current_task, mount, name, fn);
    if (result.is_error()) {
      return result.take_error();
    }
    auto [entry, _exists] = result.value();
    return fit::ok(entry);
  }

  template <typename CreateNodeFn>
  fit::result<Errno, ktl::pair<DirEntryHandle, bool>> create_entry_internal(
      const CurrentTask& current_task, const MountInfo& mount, const FsStr& name,
      CreateNodeFn&& fn) {
    static_assert(std::is_invocable_r_v<fit::result<Errno, FsNodeHandle>, CreateNodeFn,
                                        const FsNodeHandle&, const MountInfo&, const FsStr&>);

    if (DirEntry::is_reserved_name(name)) {
      return fit::error(errno(EEXIST));
    }

    // TODO: Do we need to check name for embedded NUL characters?
    if (name.size() > static_cast<size_t>(NAME_MAX)) {
      return fit::error(errno(ENAMETOOLONG));
    }
    if (starnix::contains(name, SEPARATOR)) {
      return fit::error(errno(EINVAL));
    }
    auto result = get_or_create_child(current_task, mount, name, fn);
    if (result.is_error()) {
      return result.take_error();
    }

    auto [entry, exists] = result.value();
    if (!exists) {
      // An entry was created. Update the ctime and mtime of this directory.

      // self.node.update_ctime_mtime();
      // entry.notify_creation();
    }
    return fit::ok(ktl::pair(entry, exists));
  }

 public:
  /// This is marked as test-only (private) because it sets the owner/group to root instead of the
  /// current user to save a bit of typing in tests, but this shouldn't happen silently in
  /// production.
  fit::result<Errno, DirEntryHandle> create_dir(const CurrentTask& current_task, const FsStr& name);

  // This function is for testing because it sets the owner/group to root instead of the current
  // user to save a bit of typing in tests, but this shouldn't happen silently in production.
  fit::result<Errno, DirEntryHandle> create_dir_for_testing(const CurrentTask& current_task,
                                                            const FsStr& name);

 public:
  template <typename CreateNodeFn>
  fit::result<Errno, ktl::pair<DirEntryHandle, bool>> get_or_create_child(
      const CurrentTask& current_task, const MountInfo& mount, const FsStr& name,
      CreateNodeFn&& create_fn) {
    static_assert(std::is_invocable_r_v<fit::result<Errno, FsNodeHandle>, CreateNodeFn,
                                        const FsNodeHandle&, const MountInfo&, const FsStr&>);

    ASSERT(!DirEntry::is_reserved_name(name));
    // Only directories can have children.
    if (!node_->is_dir()) {
      return fit::error(errno(ENOTDIR));
    }
    // The user must be able to search the directory (requires the EXEC permission)
    // self.node.check_access(current_task, mount, Access::EXEC)?;

    // Check if the child is already in children. In that case, we can
    // simply return the child and we do not need to call init_fn.
    auto optional_child = [&name, &children = this->children_]() -> ktl::optional<DirEntryHandle> {
      auto children_lock = children.Read();
      auto it = children_lock->find(name);
      if (it != children_lock->end()) {
        auto child = it.CopyPointer().Lock();
        if (child) {
          return child;
        }
      }
      return ktl::nullopt;
    }();

    auto result =
        [&]() -> fit::result<Errno, ktl::pair<DirEntryHandle, CreationResult<CreateNodeFn>>> {
      if (optional_child.has_value()) {
        auto c = optional_child.value();
        c->node_->fs()->did_access_dir_entry(c);
        return fit::ok(ktl::pair(c, CreationResult<CreateNodeFn>(create_fn)));
      } else {
        auto result = lock_children().get_or_create_child(current_task, mount, name, create_fn);
        if (result.is_error()) {
          return result.take_error();
        }
        auto [c, cr] = result.value();
        c->node_->fs()->purge_old_entries();
        return fit::ok(ktl::pair(c, ktl::move(cr)));
      }
    }();

    if (result.is_error()) {
      return result.take_error();
    }

    auto [child, cr] = result.value();
    auto new_result = [&]() -> fit::result<Errno, ktl::pair<DirEntryHandle, bool>> {
      return ktl::visit(
          overloaded{
              [&](const Created&) -> fit::result<Errno, ktl::pair<DirEntryHandle, bool>> {
                return fit::ok(ktl::pair(child, false));
              },
              [&](const Existed<CreateNodeFn>& e)
                  -> fit::result<Errno, ktl::pair<DirEntryHandle, bool>> {
                auto revalidate_result = child->ops_->revalidate(current_task, *child);
                if (revalidate_result.is_error()) {
                  return revalidate_result.take_error();
                }

                if (revalidate_result.value()) {
                  return fit::ok(ktl::pair(child, true));
                } else {
                  this->internal_remove_child(child.get());
                  // child.destroy(&current_task.kernel().mounts);
                  auto result =
                      lock_children().get_or_create_child(current_task, mount, name, e.create_fn);
                  if (result.is_error()) {
                    return result.take_error();
                  }
                  auto [c, cr2] = result.value();
                  c->node_->fs()->purge_old_entries();

                  return fit::ok(ktl::pair(
                      c, ktl::visit(
                             overloaded{[](const Created&) -> bool { return false; },
                                        [](const Existed<CreateNodeFn>) -> bool { return true; }},
                             cr2.variant_)));
                }
              },
          },
          cr.variant_);
    }();

    if (new_result.is_error()) {
      return result.take_error();
    }

    auto [child2, exists] = new_result.value();
    return fit::ok(ktl::pair(child2, exists));
  }

  /// This function is only useful for tests and has some oddities.
  ///
  /// For example, not all the children might have been looked up yet, which
  /// means the returned vector could be missing some names.
  ///
  /// Also, the vector might have "extra" names that are in the process of
  /// being looked up. If the lookup fails, they'll be removed.
  fbl::Vector<FsString> copy_child_names();

 private:
  void internal_remove_child(DirEntry* child);

 public:
  // C++
  ~DirEntry();
  using fbl::RefCountedUpgradeable<DirEntry>::AddRef;
  using fbl::RefCountedUpgradeable<DirEntry>::Release;
  using fbl::RefCountedUpgradeable<DirEntry>::Adopt;
  using fbl::RefCountedUpgradeable<DirEntry>::AddRefMaybeInDestructor;

  // WAVL-tree Index
  FsString GetKey() const;

 private:
  friend bool unit_testing::test_tmpfs();

  DirEntry(FsNodeHandle node, ktl::unique_ptr<DirEntryOps> ops, DirEntryState state);
};

}  // namespace starnix

#endif  // ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_DIR_ENTRY_H_
