# Copyright 2021 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# buildifier: disable=module-docstring
load("@bazel_skylib//lib:unittest.bzl", "analysistest", "asserts")
load("@fuchsia_sdk//fuchsia:defs.bzl", "fuchsia_component", "fuchsia_driver_component", "fuchsia_package", "get_component_manifests", "get_driver_component_manifests")
load("@fuchsia_sdk//fuchsia/private:providers.bzl", "FuchsiaPackageInfo")
load("//test_utils:make_file.bzl", "make_fake_component_manifest", "make_file")

## Name Tests
def _name_test_impl(ctx):
    env = analysistest.begin(ctx)

    target_under_test = analysistest.target_under_test(env)
    package_info = target_under_test[FuchsiaPackageInfo]

    if ctx.attr.package_name:
        asserts.equals(
            env,
            ctx.attr.package_name,
            package_info.package_name,
        )

    if ctx.attr.archive_name:
        asserts.equals(
            env,
            ctx.attr.archive_name,
            package_info.far_file.basename,
        )

    return analysistest.end(env)

name_test = analysistest.make(
    _name_test_impl,
    attrs = {
        "package_name": attr.string(),
        "archive_name": attr.string(),
    },
)

def _test_package_and_archive_name():
    fuchsia_package(
        name = "empty",
        tags = ["manual"],
    )

    fuchsia_package(
        name = "foo_pkg",
        package_name = "foo",
        archive_name = "some_other_archive",
        tags = ["manual"],
    )

    name_test(
        name = "name_test_empty_package",
        target_under_test = ":empty",
        package_name = "empty",
        archive_name = "empty.far",
    )

    name_test(
        name = "name_test_names_provided",
        target_under_test = ":foo_pkg",
        package_name = "foo",
        archive_name = "some_other_archive.far",
    )

def _mock_component(name, is_driver):
    make_fake_component_manifest(
        name = name + "_manifest",
        component_name = name,
        tags = ["manual"],
    )

    if is_driver:
        make_file(
            name = name + "_lib",
            filename = name + ".so",
            content = "",
        )

        make_file(
            name = name + "_bind",
            filename = name + "_bind",
            content = "",
        )

        fuchsia_driver_component(
            name = name,
            component_name = name,
            manifest = name + "_manifest",
            driver_lib = name + "_lib",
            bind_bytecode = name + "_bind",
            tags = ["manual"],
        )
    else:
        fuchsia_component(
            name = name,
            tags = ["manual"],
            component_name = name,
            manifest = name + "_manifest",
        )

def _dependencies_test_impl(ctx):
    env = analysistest.begin(ctx)

    target_under_test = analysistest.target_under_test(env)

    asserts.equals(
        env,
        sorted(ctx.attr.expected_components),
        sorted(get_component_manifests(target_under_test)),
    )

    asserts.equals(
        env,
        sorted(ctx.attr.expected_drivers),
        sorted(get_driver_component_manifests(target_under_test)),
    )

    return analysistest.end(env)

dependencies_test = analysistest.make(
    _dependencies_test_impl,
    attrs = {
        "expected_components": attr.string_list(),
        "expected_drivers": attr.string_list(),
    },
)

def _test_package_deps():
    for i in range(1, 3):
        _mock_component(
            name = "component_" + str(i),
            is_driver = False,
        )
        _mock_component(
            name = "driver_" + str(i),
            is_driver = True,
        )

    fuchsia_component(
        name = "component_with_cml",
        tags = ["manual"],
        manifest = "meta/foo.cml",
    )

    fuchsia_package(
        name = "single_component",
        tags = ["manual"],
        components = [":component_1"],
    )

    fuchsia_package(
        name = "single_driver",
        tags = ["manual"],
        components = [":driver_1"],
    )

    fuchsia_package(
        name = "composite",
        tags = ["manual"],
        components = [
            ":component_1",
            ":component_2",
            ":driver_1",
            ":driver_2",
            # test that we can pass in a plain cml file
            ":component_with_cml",
        ],
    )

    dependencies_test(
        name = "dependencies_test_single_component",
        target_under_test = ":single_component",
        expected_components = ["meta/component_1.cm"],
    )

    dependencies_test(
        name = "dependencies_test_single_driver",
        target_under_test = ":single_driver",
        expected_components = ["meta/driver_1.cm"],
        expected_drivers = ["meta/driver_1.cm"],
    )

    dependencies_test(
        name = "dependencies_test_composite",
        target_under_test = ":composite",
        expected_components = [
            "meta/component_1.cm",
            "meta/component_2.cm",
            "meta/foo.cm",
            "meta/driver_1.cm",
            "meta/driver_2.cm",
        ],
        expected_drivers = [
            "meta/driver_1.cm",
            "meta/driver_2.cm",
        ],
    )

# Entry point from the BUILD file; macro for running each test case's macro and
# declaring a test suite that wraps them together.
def fuchsia_package_test_suite(name, **kwargs):
    # Call all test functions and wrap their targets in a suite.
    _test_package_and_archive_name()
    _test_package_deps()

    native.test_suite(
        name = name,
        tests = [
            ":name_test_names_provided",
            ":name_test_empty_package",
            ":dependencies_test_single_component",
            ":dependencies_test_single_driver",
            ":dependencies_test_composite",
        ],
        **kwargs
    )
