// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fuchsia/ui/composition/cpp/fidl.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/ui/scenic/cpp/view_creation_tokens.h>
#include <lib/ui/scenic/cpp/view_identity.h>
#include <sys/types.h>
#include <zircon/status.h>

#include <cstdint>
#include <utility>

#include <zxtest/zxtest.h>

#include "src/ui/scenic/lib/allocation/buffer_collection_import_export_tokens.h"
#include "src/ui/scenic/lib/utils/helpers.h"
#include "src/ui/scenic/tests/utils/blocking_present.h"
#include "src/ui/scenic/tests/utils/scenic_ctf_test_base.h"
#include "src/ui/scenic/tests/utils/screen_capture_utils.h"
#include "src/ui/testing/util/zxtest_helpers.h"
#include "zircon/errors.h"

namespace integration_tests {

using fuchsia::ui::composition::ChildViewWatcher;
using fuchsia::ui::composition::ContentId;
using fuchsia::ui::composition::Flatland;
using fuchsia::ui::composition::FlatlandDisplay;
using fuchsia::ui::composition::FrameInfo;
using fuchsia::ui::composition::GetNextFrameArgs;
using fuchsia::ui::composition::ParentViewportWatcher;
using fuchsia::ui::composition::RegisterBufferCollectionUsages;
using fuchsia::ui::composition::ScreenCapture;
using fuchsia::ui::composition::ScreenCaptureConfig;
using fuchsia::ui::composition::ScreenCaptureError;
using fuchsia::ui::composition::TransformId;
using fuchsia::ui::composition::ViewportProperties;
using fuchsia::ui::views::ViewRef;

class ScreenCaptureIntegrationTest : public ScenicCtfTest {
 public:
  void SetUp() override {
    ScenicCtfTest::SetUp();

    LocalServiceDirectory()->Connect(sysmem_allocator_.NewRequest());

    flatland_display_ = ConnectSyncIntoRealm<fuchsia::ui::composition::FlatlandDisplay>();
    flatland_allocator_ = ConnectSyncIntoRealm<fuchsia::ui::composition::Allocator>();
    root_session_ = ConnectAsyncIntoRealm<fuchsia::ui::composition::Flatland>();

    fidl::InterfacePtr<ChildViewWatcher> child_view_watcher;
    fidl::InterfacePtr<ParentViewportWatcher> parent_viewport_watcher;
    {
      auto [child_token, parent_token] = scenic::ViewCreationTokenPair::New();
      flatland_display_->SetContent(std::move(parent_token), child_view_watcher.NewRequest());

      auto identity = scenic::NewViewIdentityOnCreation();
      root_view_ref_ = fidl::Clone(identity.view_ref);
      root_session_->CreateView2(std::move(child_token), std::move(identity), {},
                                 parent_viewport_watcher.NewRequest());
      parent_viewport_watcher->GetLayout([this](auto layout_info) {
        ASSERT_TRUE(layout_info.has_logical_size());
        const auto [width, height] = layout_info.logical_size();
        display_width_ = width;
        display_height_ = height;
        num_pixels_ = display_width_ * display_height_;
      });
    }
    BlockingPresent(this, root_session_);

    // Wait until we get the display size.
    RunLoopUntil([this] { return display_width_ != 0 && display_height_ != 0; });

    // Set up the root graph.
    fidl::InterfacePtr<ChildViewWatcher> child_view_watcher2;
    auto [child_token, parent_token] = scenic::ViewCreationTokenPair::New();
    ViewportProperties properties;
    properties.set_logical_size({display_width_, display_height_});
    const TransformId kRootTransform{.value = 1};
    const ContentId kRootContent{.value = 1};
    root_session_->CreateTransform(kRootTransform);
    root_session_->CreateViewport(kRootContent, std::move(parent_token), std::move(properties),
                                  child_view_watcher2.NewRequest());
    root_session_->SetRootTransform(kRootTransform);
    root_session_->SetContent(kRootTransform, kRootContent);
    BlockingPresent(this, root_session_);

    // Set up the child view.
    child_session_ = ConnectAsyncIntoRealm<fuchsia::ui::composition::Flatland>();
    fidl::InterfacePtr<ParentViewportWatcher> parent_viewport_watcher2;
    auto identity = scenic::NewViewIdentityOnCreation();
    auto child_view_ref = fidl::Clone(identity.view_ref);
    fuchsia::ui::composition::ViewBoundProtocols protocols;
    child_session_->CreateView2(std::move(child_token), std::move(identity), std::move(protocols),
                                parent_viewport_watcher2.NewRequest());
    child_session_->CreateTransform(kChildRootTransform);
    child_session_->SetRootTransform(kChildRootTransform);
    BlockingPresent(this, child_session_);

    // Create ScreenCapture client.
    screen_capture_ = ConnectSyncIntoRealm<fuchsia::ui::composition::ScreenCapture>();
  }

  // This function calls GetNextFrame().
  fpromise::result<FrameInfo, ScreenCaptureError> CaptureScreen(
      fuchsia::ui::composition::ScreenCaptureSyncPtr& screencapturer) {
    zx::event event;
    zx::event dup;
    zx_status_t status = zx::event::create(0, &event);
    EXPECT_EQ(status, ZX_OK);
    event.duplicate(ZX_RIGHT_SAME_RIGHTS, &dup);

    GetNextFrameArgs gnf_args;
    gnf_args.set_event(std::move(dup));

    fuchsia::ui::composition::ScreenCapture_GetNextFrame_Result result;
    status = screencapturer->GetNextFrame(std::move(gnf_args), &result);
    EXPECT_EQ(status, ZX_OK);

    fpromise::result<FrameInfo, ScreenCaptureError> response = std::move(result);

    if (response.is_ok()) {
      zx::duration kEventDelay = zx::msec(5000);
      status = event.wait_one(ZX_EVENT_SIGNALED, zx::deadline_after(kEventDelay), nullptr);
      EXPECT_EQ(status, ZX_OK);
    }

    return response;
  }

  const TransformId kChildRootTransform{.value = 1};
  static constexpr zx::duration kEventDelay = zx::msec(5000);

  fuchsia::sysmem2::AllocatorSyncPtr sysmem_allocator_;
  fuchsia::ui::composition::AllocatorSyncPtr flatland_allocator_;
  fuchsia::ui::composition::FlatlandDisplaySyncPtr flatland_display_;
  fuchsia::ui::composition::FlatlandPtr root_session_;
  fuchsia::ui::composition::FlatlandPtr child_session_;
  fuchsia::ui::composition::ScreenCaptureSyncPtr screen_capture_;
  fuchsia::ui::views::ViewRef root_view_ref_;

  uint32_t display_width_ = 0;
  uint32_t display_height_ = 0;
  uint32_t num_pixels_ = 0;
};

TEST_F(ScreenCaptureIntegrationTest, EmptyScreenshot) {
  // Detach |flatland_display_| from the scene graph.
  flatland_display_.Unbind();

  const uint32_t render_target_width = display_width_;
  const uint32_t render_target_height = display_height_;

  // Create buffer collection to render into for GetNextFrame().
  allocation::BufferCollectionImportExportTokens scr_ref_pair =
      allocation::BufferCollectionImportExportTokens::New();

  fuchsia::sysmem2::BufferCollectionInfo sc_buffer_collection_info =
      CreateBufferCollectionInfoWithConstraints(
          utils::CreateDefaultConstraints(/*buffer_count=*/1, render_target_width,
                                          render_target_height),
          std::move(scr_ref_pair.export_token), flatland_allocator_.get(), sysmem_allocator_.get(),
          RegisterBufferCollectionUsages::SCREENSHOT);

  // Configure buffers in ScreenCapture client.
  ScreenCaptureConfig sc_args;
  sc_args.set_import_token(std::move(scr_ref_pair.import_token));
  sc_args.set_buffer_count(static_cast<uint32_t>(sc_buffer_collection_info.buffers().size()));
  sc_args.set_size({render_target_width, render_target_height});

  fuchsia::ui::composition::ScreenCapture_Configure_Result config_res;
  auto state = screen_capture_->Configure(std::move(sc_args), &config_res);
  ASSERT_EQ(ZX_OK, state);
  ASSERT_FALSE(config_res.is_err());

  const auto& cs_result = CaptureScreen(screen_capture_);
  EXPECT_FALSE(cs_result.is_error());
  const auto& read_values =
      ExtractScreenCapture(cs_result.value().buffer_id(), sc_buffer_collection_info, kBytesPerPixel,
                           render_target_width, render_target_height);

  // Compare read and write values.
  uint32_t num_zero = 0;
  for (size_t i = 0; i < read_values.size(); i += kBytesPerPixel) {
    if (PixelEquals(&read_values[i], kZero))
      num_zero++;
  }
  EXPECT_EQ(num_zero, num_pixels_);
}

TEST_F(ScreenCaptureIntegrationTest, SingleColorUnrotatedScreenshot) {
  const uint32_t image_width = display_width_;
  const uint32_t image_height = display_height_;
  const uint32_t render_target_width = display_width_;
  const uint32_t render_target_height = display_height_;

  // Create Buffer Collection for image to add to scene graph.
  allocation::BufferCollectionImportExportTokens ref_pair =
      allocation::BufferCollectionImportExportTokens::New();

  fuchsia::sysmem2::BufferCollectionInfo buffer_collection_info =
      CreateBufferCollectionInfoWithConstraints(
          utils::CreateDefaultConstraints(/*buffer_count=*/1, image_width, image_height),
          std::move(ref_pair.export_token), flatland_allocator_.get(), sysmem_allocator_.get(),
          RegisterBufferCollectionUsages::DEFAULT);

  std::vector<uint8_t> write_values;
  for (uint32_t i = 0; i < num_pixels_; ++i) {
    write_values.insert(write_values.end(), kRed, kRed + kBytesPerPixel);
  }

  WriteToSysmemBuffer(write_values, buffer_collection_info, 0, kBytesPerPixel, image_width,
                      image_height);

  GenerateImageForFlatlandInstance(0, child_session_, kChildRootTransform,
                                   std::move(ref_pair.import_token), {image_width, image_height},
                                   {0, 0}, 2, 2);
  BlockingPresent(this, child_session_);

  // The scene graph is now ready for screencapturing!

  // Create buffer collection to render into for GetNextFrame().
  allocation::BufferCollectionImportExportTokens scr_ref_pair =
      allocation::BufferCollectionImportExportTokens::New();

  fuchsia::sysmem2::BufferCollectionInfo sc_buffer_collection_info =
      CreateBufferCollectionInfoWithConstraints(
          utils::CreateDefaultConstraints(/*buffer_count=*/1, render_target_width,
                                          render_target_height),
          std::move(scr_ref_pair.export_token), flatland_allocator_.get(), sysmem_allocator_.get(),
          RegisterBufferCollectionUsages::SCREENSHOT);

  // Configure buffers in ScreenCapture client.
  ScreenCaptureConfig sc_args;
  sc_args.set_import_token(std::move(scr_ref_pair.import_token));
  sc_args.set_buffer_count(static_cast<uint32_t>(sc_buffer_collection_info.buffers().size()));
  sc_args.set_size({render_target_width, render_target_height});

  fuchsia::ui::composition::ScreenCapture_Configure_Result config_res;
  auto state = screen_capture_->Configure(std::move(sc_args), &config_res);
  ASSERT_EQ(ZX_OK, state);
  ASSERT_FALSE(config_res.is_err());

  // Take Screenshot!
  const auto& cs_result = CaptureScreen(screen_capture_);
  EXPECT_FALSE(cs_result.is_error());
  const auto& read_values =
      ExtractScreenCapture(cs_result.value().buffer_id(), sc_buffer_collection_info, kBytesPerPixel,
                           render_target_width, render_target_height);

  EXPECT_EQ(read_values.size(), write_values.size());

  // Compare read and write values.
  uint32_t num_red = 0;

  for (size_t i = 0; i < read_values.size(); i += kBytesPerPixel) {
    if (PixelEquals(&read_values[i], kRed))
      num_red++;
  }

  EXPECT_EQ(num_red, num_pixels_);
}

// Creates this image:
//          RRRRRRRR
//          RRRRRRRR
//          GGGGGGGG
//          GGGGGGGG
//
// Rotates into this image:
//          GGGGGGGG
//          GGGGGGGG
//          RRRRRRRR
//          RRRRRRRR
TEST_F(ScreenCaptureIntegrationTest, MultiColor180DegreeRotationScreenshot) {
  const uint32_t image_width = display_width_;
  const uint32_t image_height = display_height_;
  const uint32_t render_target_width = display_width_;
  const uint32_t render_target_height = display_height_;

  // Create Buffer Collection for image#1 to add to scene graph.
  allocation::BufferCollectionImportExportTokens ref_pair =
      allocation::BufferCollectionImportExportTokens::New();

  fuchsia::sysmem2::BufferCollectionInfo buffer_collection_info =
      CreateBufferCollectionInfoWithConstraints(
          utils::CreateDefaultConstraints(/*buffer_count=*/1, image_width, image_height),
          std::move(ref_pair.export_token), flatland_allocator_.get(), sysmem_allocator_.get(),
          RegisterBufferCollectionUsages::DEFAULT);

  // Write the image with half green, half red
  std::vector<uint8_t> write_values;
  const uint32_t pixel_color_count = num_pixels_ / 2;

  for (uint32_t i = 0; i < pixel_color_count; ++i) {
    AppendPixel(&write_values, kRed);
  }
  for (uint32_t i = 0; i < pixel_color_count; ++i) {
    AppendPixel(&write_values, kGreen);
  }
  WriteToSysmemBuffer(write_values, buffer_collection_info, 0, kBytesPerPixel, image_width,
                      image_height);

  GenerateImageForFlatlandInstance(0, child_session_, kChildRootTransform,
                                   std::move(ref_pair.import_token), {image_width, image_height},
                                   {0, 0}, 2, 2);

  BlockingPresent(this, child_session_);

  // The scene graph is now ready for screenshotting!

  // Create buffer collection to render into for GetNextFrame().
  allocation::BufferCollectionImportExportTokens scr_ref_pair =
      allocation::BufferCollectionImportExportTokens::New();

  fuchsia::sysmem2::BufferCollectionInfo sc_buffer_collection_info =
      CreateBufferCollectionInfoWithConstraints(
          utils::CreateDefaultConstraints(/*buffer_count=*/1, render_target_width,
                                          render_target_height),
          std::move(scr_ref_pair.export_token), flatland_allocator_.get(), sysmem_allocator_.get(),
          RegisterBufferCollectionUsages::SCREENSHOT);

  // Configure buffers in ScreenCapture client.
  ScreenCaptureConfig sc_args;
  sc_args.set_import_token(std::move(scr_ref_pair.import_token));
  sc_args.set_buffer_count(static_cast<uint32_t>(sc_buffer_collection_info.buffers().size()));
  sc_args.set_size({render_target_width, render_target_height});
  sc_args.set_rotation(fuchsia::ui::composition::Rotation::CW_180_DEGREES);

  fuchsia::ui::composition::ScreenCapture_Configure_Result config_res;
  auto state = screen_capture_->Configure(std::move(sc_args), &config_res);
  ASSERT_EQ(ZX_OK, state);
  ASSERT_FALSE(config_res.is_err());

  // Take Screenshot!
  const auto& cs_result = CaptureScreen(screen_capture_);
  EXPECT_FALSE(cs_result.is_error());
  const auto& read_values =
      ExtractScreenCapture(cs_result.value().buffer_id(), sc_buffer_collection_info, kBytesPerPixel,
                           render_target_width, render_target_height);

  EXPECT_EQ(read_values.size(), write_values.size());

  // Compare read and write values.
  uint32_t num_green = 0;
  uint32_t num_red = 0;

  for (size_t i = 0; i < read_values.size(); i += kBytesPerPixel) {
    if (PixelEquals(&read_values[i], kGreen)) {
      num_green++;
      EXPECT_TRUE(PixelEquals(&write_values[i], kRed));
    } else if (PixelEquals(&read_values[i], kRed)) {
      num_red++;
      EXPECT_TRUE(PixelEquals(&write_values[i], kGreen));
    }
  }

  EXPECT_EQ(num_green, pixel_color_count);
  // TODO(https://fxbug.dev/42067818): Switch to exact comparisons after Astro precision issues are
  // resolved.
  EXPECT_NEAR(num_red, pixel_color_count, display_width_);
}

// Creates this image:
//          RRRRRGGGGG
//          RRRRRGGGGG
//          YYYYYBBBBB
//          YYYYYBBBBB
//
// Rotates into this image:
//          YYRR
//          YYRR
//          YYRR
//          YYRR
//          YYRR
//          BBGG
//          BBGG
//          BBGG
//          BBGG
//          BBGG
TEST_F(ScreenCaptureIntegrationTest, MultiColor90DegreeRotationScreenshot) {
  const uint32_t image_width = display_width_;
  const uint32_t image_height = display_height_;
  const uint32_t render_target_width = display_height_;
  const uint32_t render_target_height = display_width_;

  // Create Buffer Collection for image#1 to add to scene graph.
  allocation::BufferCollectionImportExportTokens ref_pair =
      allocation::BufferCollectionImportExportTokens::New();

  fuchsia::sysmem2::BufferCollectionInfo buffer_collection_info =
      CreateBufferCollectionInfoWithConstraints(
          utils::CreateDefaultConstraints(/*buffer_count=*/1, image_width, image_height),
          std::move(ref_pair.export_token), flatland_allocator_.get(), sysmem_allocator_.get(),
          RegisterBufferCollectionUsages::DEFAULT);

  // Write the image with the color scheme displayed in ASCII above.
  std::vector<uint8_t> write_values;

  uint32_t red_pixel_count = 0;
  uint32_t green_pixel_count = 0;
  uint32_t blue_pixel_count = 0;
  uint32_t yellow_pixel_count = 0;
  const uint32_t pixel_color_count = num_pixels_ / 4;

  for (uint32_t i = 0; i < num_pixels_; ++i) {
    uint32_t row = i / image_width;
    uint32_t col = i % image_width;

    // Top-left quadrant
    if (row < image_height / 2 && col < image_width / 2) {
      AppendPixel(&write_values, kRed);
      ++red_pixel_count;
    }
    // Top-right quadrant
    else if (row < image_height / 2 && col >= image_width / 2) {
      AppendPixel(&write_values, kGreen);
      ++green_pixel_count;
    }
    // Bottom-right quadrant
    else if (row >= image_height / 2 && col >= image_width / 2) {
      AppendPixel(&write_values, kBlue);
      ++blue_pixel_count;
    }
    // Bottom-left quadrant
    else if (row >= image_height / 2 && col < image_width / 2) {
      AppendPixel(&write_values, kYellow);
      ++yellow_pixel_count;
    }
  }

  EXPECT_EQ(red_pixel_count, pixel_color_count);
  EXPECT_EQ(green_pixel_count, pixel_color_count);
  EXPECT_EQ(blue_pixel_count, pixel_color_count);
  EXPECT_EQ(yellow_pixel_count, pixel_color_count);

  WriteToSysmemBuffer(write_values, buffer_collection_info, 0, kBytesPerPixel, image_width,
                      image_height);

  GenerateImageForFlatlandInstance(0, child_session_, kChildRootTransform,
                                   std::move(ref_pair.import_token), {image_width, image_height},
                                   {0, 0}, 2, 2);
  BlockingPresent(this, child_session_);

  // The scene graph is now ready for screenshotting!

  // Create buffer collection to render into for GetNextFrame().
  allocation::BufferCollectionImportExportTokens scr_ref_pair =
      allocation::BufferCollectionImportExportTokens::New();

  fuchsia::sysmem2::BufferCollectionInfo sc_buffer_collection_info =
      CreateBufferCollectionInfoWithConstraints(
          utils::CreateDefaultConstraints(/*buffer_count=*/1, render_target_width,
                                          render_target_height),
          std::move(scr_ref_pair.export_token), flatland_allocator_.get(), sysmem_allocator_.get(),
          RegisterBufferCollectionUsages::SCREENSHOT);

  // Configure buffers in ScreenCapture client.
  ScreenCaptureConfig sc_args;
  sc_args.set_import_token(std::move(scr_ref_pair.import_token));
  sc_args.set_buffer_count(static_cast<uint32_t>(sc_buffer_collection_info.buffers().size()));
  sc_args.set_size({render_target_width, render_target_height});
  sc_args.set_rotation(fuchsia::ui::composition::Rotation::CW_90_DEGREES);

  fuchsia::ui::composition::ScreenCapture_Configure_Result config_res;
  auto state = screen_capture_->Configure(std::move(sc_args), &config_res);
  ASSERT_EQ(ZX_OK, state);
  ASSERT_FALSE(config_res.is_err());

  // Take Screenshot!
  const auto& cs_result = CaptureScreen(screen_capture_);
  EXPECT_FALSE(cs_result.is_error());
  const auto& read_values =
      ExtractScreenCapture(cs_result.value().buffer_id(), sc_buffer_collection_info, kBytesPerPixel,
                           render_target_width, render_target_height);

  EXPECT_EQ(read_values.size(), write_values.size());

  // Compare read and write values for each quadrant.
  uint32_t top_left_correct = 0;
  uint32_t top_right_correct = 0;
  uint32_t bottom_right_correct = 0;
  uint32_t bottom_left_correct = 0;

  for (uint32_t i = 0; i < num_pixels_; ++i) {
    uint32_t row = i / render_target_width;
    uint32_t col = i % render_target_width;
    const uint8_t* read_value = &read_values[i * kBytesPerPixel];

    // Top-left quadrant
    if (row < render_target_height / 2 && col < render_target_width / 2) {
      if (PixelEquals(read_value, kYellow))
        top_left_correct++;
    }
    // Top-right quadrant
    else if (row < render_target_height / 2 && col >= render_target_width / 2) {
      if (PixelEquals(read_value, kRed))
        top_right_correct++;
    }
    // Bottom-right quadrant
    else if (row >= render_target_height / 2 && col >= render_target_width / 2) {
      if (PixelEquals(read_value, kGreen))
        bottom_right_correct++;
    }
    // Bottom-left quadrant
    else if (row >= render_target_height / 2 && col < render_target_width / 2) {
      if (PixelEquals(read_value, kBlue))
        bottom_left_correct++;
    }
  }

  // TODO(https://fxbug.dev/42067818): Switch to exact comparisons after Astro precision issues are
  // resolved.
  EXPECT_NEAR(top_left_correct, pixel_color_count, display_width_);
  EXPECT_NEAR(top_right_correct, pixel_color_count, display_width_);
  EXPECT_NEAR(bottom_left_correct, pixel_color_count, display_width_);
  EXPECT_NEAR(bottom_right_correct, pixel_color_count, display_width_);
}

// Creates this image:
//          RRRRRGGGGG
//          RRRRRGGGGG
//          YYYYYBBBBB
//          YYYYYBBBBB
//
// Rotates into this image:
//          GGBB
//          GGBB
//          GGBB
//          GGBB
//          GGBB
//          RRYY
//          RRYY
//          RRYY
//          RRYY
//          RRYY
TEST_F(ScreenCaptureIntegrationTest, MultiColor270DegreeRotationScreenshot) {
  const uint32_t image_width = display_width_;
  const uint32_t image_height = display_height_;
  const uint32_t render_target_width = display_height_;
  const uint32_t render_target_height = display_width_;

  // Create Buffer Collection for image#1 to add to scene graph.
  allocation::BufferCollectionImportExportTokens ref_pair =
      allocation::BufferCollectionImportExportTokens::New();

  fuchsia::sysmem2::BufferCollectionInfo buffer_collection_info =
      CreateBufferCollectionInfoWithConstraints(
          utils::CreateDefaultConstraints(/*buffer_count=*/1, image_width, image_height),
          std::move(ref_pair.export_token), flatland_allocator_.get(), sysmem_allocator_.get(),
          RegisterBufferCollectionUsages::DEFAULT);

  // Write the image with the color scheme displayed in ASCII above.
  std::vector<uint8_t> write_values;

  uint32_t red_pixel_count = 0;
  uint32_t green_pixel_count = 0;
  uint32_t blue_pixel_count = 0;
  uint32_t yellow_pixel_count = 0;
  const uint32_t pixel_color_count = num_pixels_ / 4;

  for (uint32_t i = 0; i < num_pixels_; ++i) {
    uint32_t row = i / image_width;
    uint32_t col = i % image_width;

    // Top-left quadrant
    if (row < image_height / 2 && col < image_width / 2) {
      AppendPixel(&write_values, kRed);
      ++red_pixel_count;
    }
    // Top-right quadrant
    else if (row < image_height / 2 && col >= image_width / 2) {
      AppendPixel(&write_values, kGreen);
      ++green_pixel_count;
    }
    // Bottom-right quadrant
    else if (row >= image_height / 2 && col >= image_width / 2) {
      AppendPixel(&write_values, kBlue);
      ++blue_pixel_count;
    }
    // Bottom-left quadrant
    else if (row >= image_height / 2 && col < image_width / 2) {
      AppendPixel(&write_values, kYellow);
      ++yellow_pixel_count;
    }
  }

  EXPECT_EQ(red_pixel_count, pixel_color_count);
  EXPECT_EQ(green_pixel_count, pixel_color_count);
  EXPECT_EQ(blue_pixel_count, pixel_color_count);
  EXPECT_EQ(yellow_pixel_count, pixel_color_count);

  WriteToSysmemBuffer(write_values, buffer_collection_info, 0, kBytesPerPixel, image_width,
                      image_height);

  GenerateImageForFlatlandInstance(0, child_session_, kChildRootTransform,
                                   std::move(ref_pair.import_token), {image_width, image_height},
                                   {0, 0}, 2, 2);
  BlockingPresent(this, child_session_);

  // The scene graph is now ready for screenshotting!

  // Create buffer collection to render into for GetNextFrame().
  allocation::BufferCollectionImportExportTokens scr_ref_pair =
      allocation::BufferCollectionImportExportTokens::New();

  fuchsia::sysmem2::BufferCollectionInfo sc_buffer_collection_info =
      CreateBufferCollectionInfoWithConstraints(
          utils::CreateDefaultConstraints(/*buffer_count=*/1, render_target_width,
                                          render_target_height),
          std::move(scr_ref_pair.export_token), flatland_allocator_.get(), sysmem_allocator_.get(),
          RegisterBufferCollectionUsages::SCREENSHOT);

  // Configure buffers in ScreenCapture client.
  ScreenCaptureConfig sc_args;
  sc_args.set_import_token(std::move(scr_ref_pair.import_token));
  sc_args.set_buffer_count(static_cast<uint32_t>(sc_buffer_collection_info.buffers().size()));
  sc_args.set_size({render_target_width, render_target_height});
  sc_args.set_rotation(fuchsia::ui::composition::Rotation::CW_270_DEGREES);

  fuchsia::ui::composition::ScreenCapture_Configure_Result config_res;
  auto state = screen_capture_->Configure(std::move(sc_args), &config_res);
  ASSERT_EQ(ZX_OK, state);
  ASSERT_FALSE(config_res.is_err());

  // Take Screenshot!
  const auto& cs_result = CaptureScreen(screen_capture_);
  EXPECT_FALSE(cs_result.is_error());
  const auto& read_values =
      ExtractScreenCapture(cs_result.value().buffer_id(), sc_buffer_collection_info, kBytesPerPixel,
                           render_target_width, render_target_height);

  EXPECT_EQ(read_values.size(), write_values.size());

  // Compare read and write values for each quadrant.
  uint32_t top_left_correct = 0;
  uint32_t top_right_correct = 0;
  uint32_t bottom_right_correct = 0;
  uint32_t bottom_left_correct = 0;

  for (uint32_t i = 0; i < num_pixels_; ++i) {
    uint32_t row = i / render_target_width;
    uint32_t col = i % render_target_width;
    const uint8_t* read_value = &read_values[i * kBytesPerPixel];

    // Top-left quadrant
    if (row < render_target_height / 2 && col < render_target_width / 2) {
      if (PixelEquals(read_value, kGreen))
        top_left_correct++;
    }
    // Top-right quadrant
    else if (row < render_target_height / 2 && col >= render_target_width / 2) {
      if (PixelEquals(read_value, kBlue))
        top_right_correct++;
    }
    // Bottom-right quadrant
    else if (row >= render_target_height / 2 && col >= render_target_width / 2) {
      if (PixelEquals(read_value, kYellow))
        bottom_right_correct++;
    }
    // Bottom-left quadrant
    else if (row >= render_target_height / 2 && col < render_target_width / 2) {
      if (PixelEquals(read_value, kRed))
        bottom_left_correct++;
    }
  }

  // TODO(https://fxbug.dev/42067818): Switch to exact comparisons after Astro precision issues are
  // resolved.
  EXPECT_NEAR(top_left_correct, pixel_color_count, display_width_);
  EXPECT_NEAR(top_right_correct, pixel_color_count, display_width_);
  EXPECT_NEAR(bottom_left_correct, pixel_color_count, display_width_);
  EXPECT_NEAR(bottom_right_correct, pixel_color_count, display_width_);
}

TEST_F(ScreenCaptureIntegrationTest, FilledRectScreenshot) {
  const uint32_t image_width = display_width_;
  const uint32_t image_height = display_height_;
  const uint32_t render_target_width = display_width_;
  const uint32_t render_target_height = display_height_;

  const ContentId kFilledRectId = {1};
  const TransformId kTransformId = {2};

  // Create a fuchsia colored rectangle.
  child_session_->CreateFilledRect(kFilledRectId);
  child_session_->SetSolidFill(kFilledRectId, {1, 0, 1, 1}, {image_width, image_height});

  // Associate the rect with a transform.
  child_session_->CreateTransform(kTransformId);
  child_session_->SetContent(kTransformId, kFilledRectId);

  // Attach the transform to the scene
  child_session_->AddChild(kChildRootTransform, kTransformId);
  BlockingPresent(this, child_session_);

  // The scene graph is now ready for screencapturing!

  // Create buffer collection to render into for GetNextFrame().
  allocation::BufferCollectionImportExportTokens scr_ref_pair =
      allocation::BufferCollectionImportExportTokens::New();

  fuchsia::sysmem2::BufferCollectionInfo sc_buffer_collection_info =
      CreateBufferCollectionInfoWithConstraints(
          utils::CreateDefaultConstraints(/*buffer_count=*/1, render_target_width,
                                          render_target_height),
          std::move(scr_ref_pair.export_token), flatland_allocator_.get(), sysmem_allocator_.get(),
          RegisterBufferCollectionUsages::SCREENSHOT);

  // Configure buffers in ScreenCapture client.
  ScreenCaptureConfig sc_args;
  sc_args.set_import_token(std::move(scr_ref_pair.import_token));
  sc_args.set_size({render_target_width, render_target_height});
  sc_args.set_buffer_count(static_cast<uint32_t>(sc_buffer_collection_info.buffers().size()));

  fuchsia::ui::composition::ScreenCapture_Configure_Result config_res;
  auto state = screen_capture_->Configure(std::move(sc_args), &config_res);
  ASSERT_EQ(ZX_OK, state);
  ASSERT_FALSE(config_res.is_err());

  // Take Screenshot!
  const auto& cs_result = CaptureScreen(screen_capture_);
  EXPECT_FALSE(cs_result.is_error());
  const auto& read_values =
      ExtractScreenCapture(cs_result.value().buffer_id(), sc_buffer_collection_info, kBytesPerPixel,
                           render_target_width, render_target_height);

  EXPECT_EQ(read_values.size(), num_pixels_ * kBytesPerPixel);

  // Compare read and write values.
  uint32_t num_fuchsia_count = 0;
  static constexpr uint8_t kFuchsia[] = {255, 0, 255, 255};

  for (size_t i = 0; i < read_values.size(); i += kBytesPerPixel) {
    if (PixelEquals(&read_values[i], kFuchsia))
      num_fuchsia_count++;
  }

  EXPECT_EQ(num_fuchsia_count, num_pixels_);
}

TEST_F(ScreenCaptureIntegrationTest, ChangeFilledRectScreenshots) {
  const uint32_t image_width = display_width_;
  const uint32_t image_height = display_height_;
  const uint32_t render_target_width = display_width_;
  const uint32_t render_target_height = display_height_;

  const ContentId kFilledRectId = {1};
  const TransformId kTransformId = {2};

  // Create a red rectangle.
  child_session_->CreateFilledRect(kFilledRectId);
  // Set as RGBA. Corresponds to kRed.
  child_session_->SetSolidFill(kFilledRectId, {1, 0, 0, 1}, {image_width, image_height});

  // Associate the rect with a transform.
  child_session_->CreateTransform(kTransformId);
  child_session_->SetContent(kTransformId, kFilledRectId);

  // Attach the transform to the scene
  child_session_->AddChild(kChildRootTransform, kTransformId);
  BlockingPresent(this, child_session_);

  // The scene graph is now ready for screencapturing!

  // Create buffer collection to render into for GetNextFrame().
  allocation::BufferCollectionImportExportTokens scr_ref_pair =
      allocation::BufferCollectionImportExportTokens::New();

  fuchsia::sysmem2::BufferCollectionInfo sc_buffer_collection_info =
      CreateBufferCollectionInfoWithConstraints(
          utils::CreateDefaultConstraints(/*buffer_count=*/2, render_target_width,
                                          render_target_height),
          std::move(scr_ref_pair.export_token), flatland_allocator_.get(), sysmem_allocator_.get(),
          RegisterBufferCollectionUsages::SCREENSHOT);

  // Configure buffers in ScreenCapture client.
  ScreenCaptureConfig sc_args;
  sc_args.set_import_token(std::move(scr_ref_pair.import_token));
  sc_args.set_size({render_target_width, render_target_height});
  sc_args.set_buffer_count(static_cast<uint32_t>(sc_buffer_collection_info.buffers().size()));

  fuchsia::ui::composition::ScreenCapture_Configure_Result config_res;
  auto state = screen_capture_->Configure(std::move(sc_args), &config_res);
  ASSERT_EQ(ZX_OK, state);
  ASSERT_FALSE(config_res.is_err());

  // Take Screenshot!
  const auto& cs_result = CaptureScreen(screen_capture_);
  EXPECT_FALSE(cs_result.is_error());
  const auto& read_values =
      ExtractScreenCapture(cs_result.value().buffer_id(), sc_buffer_collection_info, kBytesPerPixel,
                           render_target_width, render_target_height);

  EXPECT_EQ(read_values.size(), num_pixels_ * kBytesPerPixel);

  // Compare read and write values.
  uint32_t num_red_count = 0;

  for (size_t i = 0; i < read_values.size(); i += kBytesPerPixel) {
    if (PixelEquals(&read_values[i], kRed))
      num_red_count++;
  }

  EXPECT_EQ(num_red_count, num_pixels_);

  // Now change the color of the screen.

  const ContentId kFilledRectId2 = {2};
  const TransformId kTransformId2 = {3};

  // Create a blue rectangle.
  child_session_->CreateFilledRect(kFilledRectId2);
  // Set as RGBA. Corresponds to kBlue.
  child_session_->SetSolidFill(kFilledRectId2, {0, 0, 1, 1}, {image_width, image_height});

  // Associate the rect with a transform.
  child_session_->CreateTransform(kTransformId2);
  child_session_->SetContent(kTransformId2, kFilledRectId2);

  // Attach the transform to the scene
  child_session_->AddChild(kChildRootTransform, kTransformId2);
  BlockingPresent(this, child_session_);

  // The scene graph is now ready for screencapturing!

  // Take Screenshot!
  const auto& cs_result2 = CaptureScreen(screen_capture_);
  EXPECT_FALSE(cs_result2.is_error());
  const auto& read_values2 =
      ExtractScreenCapture(cs_result2.value().buffer_id(), sc_buffer_collection_info,
                           kBytesPerPixel, render_target_width, render_target_height);
  EXPECT_EQ(read_values2.size(), num_pixels_ * kBytesPerPixel);

  // Compare read and write values.
  uint32_t num_blue_count = 0;

  for (size_t i = 0; i < read_values2.size(); i += kBytesPerPixel) {
    if (PixelEquals(&read_values2[i], kBlue))
      num_blue_count++;
  }

  EXPECT_EQ(num_blue_count, num_pixels_);
}

}  // namespace integration_tests
