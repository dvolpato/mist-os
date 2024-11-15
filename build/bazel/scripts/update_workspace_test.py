#!/usr/bin/env python3

# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import os
import tempfile
import unittest
from typing import Callable

import update_workspace
from parameterized import parameterized


def _empty_dir(root: str) -> tuple[str, tuple[str, ...]]:
    dir = tempfile.mkdtemp(dir=root)
    return dir, (dir,)


def _dir_with_file(root: str) -> tuple[str, tuple[str, ...]]:
    dir = tempfile.mkdtemp(dir=root)
    _, f = tempfile.mkstemp(dir=dir)
    return dir, (dir, f)


def _dir_with_symlink(root: str) -> tuple[str, tuple[str, ...]]:
    dir = tempfile.mkdtemp(dir=root)
    _, f = tempfile.mkstemp(dir=dir)
    l = f"{f}_link"
    os.symlink(f, l)
    return dir, (dir, f, l)


def _dir_with_subdir_symlink(root: str) -> tuple[str, tuple[str, ...]]:
    dir = tempfile.mkdtemp(dir=root)
    subdir = tempfile.mkdtemp(dir=dir)
    _, f = tempfile.mkstemp(dir=subdir)
    l = f"{subdir}_link"
    os.symlink(subdir, l, target_is_directory=True)
    return dir, (dir, subdir, f, l)


class RemoveDirTests(unittest.TestCase):
    @parameterized.expand(
        (
            ("empty_dir", _empty_dir),
            ("dir_with_file", _dir_with_file),
            ("dir_with_symlink", _dir_with_symlink),
            ("dir_with_subdir_symlink", _dir_with_subdir_symlink),
        )
    )
    def test_remove_dir(
        self,
        name: str,
        create_test_dir: Callable[..., tuple[str, tuple[str, ...]]],
    ) -> None:
        # Create all test dirs and files under a top-level temporary directory so
        # they are properly cleaned up on test failures as well.
        with tempfile.TemporaryDirectory() as root:
            top_dir, all_paths = create_test_dir(root)

            for p in all_paths:
                self.assertTrue(
                    os.path.exists(p),
                    f"setup failure: {p} does not exist after test dir creation",
                )

            update_workspace.remove_dir(top_dir)

            for p in all_paths:
                self.assertFalse(
                    os.path.exists(p),
                    f"{p} still exists after remove_dir({top_dir})",
                )


if __name__ == "__main__":
    unittest.main()
