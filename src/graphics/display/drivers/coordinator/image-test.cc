// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/coordinator/image.h"

#include <lib/async-loop/loop.h>
#include <lib/async-testing/test_loop.h>
#include <lib/async/cpp/task.h>
#include <lib/async/cpp/wait.h>
#include <lib/async/default.h>
#include <lib/fit/defer.h>

#include <fbl/auto_lock.h>
#include <fbl/ref_ptr.h>
#include <gtest/gtest.h>

#include "src/graphics/display/drivers/coordinator/fence.h"
#include "src/graphics/display/drivers/coordinator/post-display-task.h"
#include "src/graphics/display/drivers/coordinator/testing/base.h"
#include "src/graphics/display/drivers/fake/fake-display.h"
#include "src/graphics/display/lib/api-types-cpp/driver-image-id.h"
#include "src/graphics/display/lib/api-types-cpp/image-metadata.h"
#include "src/graphics/display/lib/driver-utils/post-task.h"
#include "src/lib/testing/predicates/status.h"

namespace display {

class ImageTest : public TestBase, public FenceCallback {
 public:
  void OnFenceFired(FenceReference* f) override {}
  void OnRefForFenceDead(Fence* fence) override { fence->OnRefDead(); }

  fbl::RefPtr<Image> ImportImage(zx::vmo vmo, const ImageMetadata& image_metadata) {
    zx::result<DriverImageId> import_result =
        display()->ImportVmoImageForTesting(std::move(vmo), /*offset=*/0);
    if (!import_result.is_ok()) {
      return nullptr;
    }

    fbl::RefPtr<Image> image = fbl::AdoptRef(
        new Image(controller(), image_metadata, import_result.value(), nullptr, ClientId(1)));
    image->id = next_image_id_++;
    return image;
  }

 private:
  ImageId next_image_id_ = ImageId(1);
};

TEST_F(ImageTest, MultipleAcquiresAllowed) {
  zx::vmo vmo;
  ASSERT_OK(zx::vmo::create(1024 * 600 * 4, 0u, &vmo));
  static constexpr ImageMetadata image_metadata({
      .width = 1024,
      .height = 600,
      .tiling_type = kImageTilingTypeLinear,
  });
  fbl::RefPtr<Image> image = ImportImage(std::move(vmo), image_metadata);

  EXPECT_TRUE(image->Acquire());
  image->DiscardAcquire();
  EXPECT_TRUE(image->Acquire());
  image->EarlyRetire();
}

TEST_F(ImageTest, RetiredImagesAreAlwaysUsable) {
  async::Loop loop(&kAsyncLoopConfigNeverAttachToThread);

  zx::vmo vmo;
  ASSERT_OK(zx::vmo::create(1024 * 600 * 4, 0u, &vmo));
  static constexpr ImageMetadata image_metadata({
      .width = 1024,
      .height = 600,
      .tiling_type = kImageTilingTypeLinear,
  });
  fbl::RefPtr<Image> image = ImportImage(std::move(vmo), image_metadata);
  auto image_cleanup = fit::defer([image]() {
    fbl::AutoLock l(image->mtx());
    image->ResetFences();
  });

  zx::event signal_event;
  ASSERT_OK(zx::event::create(0, &signal_event));
  zx::event signal_event_dup;
  signal_event.duplicate(ZX_RIGHT_SAME_RIGHTS, &signal_event_dup);
  constexpr EventId kEventId(1);
  auto signal_fence =
      fbl::AdoptRef(new Fence(this, loop.dispatcher(), kEventId, std::move(signal_event_dup)));
  signal_fence->CreateRef();
  auto signal_cleanup = fit::defer([signal_fence]() { signal_fence->ClearRef(); });

  zx::port signal_port;
  ASSERT_OK(zx::port::create(0, &signal_port));
  constexpr size_t kNumIterations = 1000;
  size_t failures = 0;
  size_t attempts = kNumIterations;
  size_t retire_count = 0;
  // Miniature naive render loop. Repeatedly acquire the image, run its lifecycle on another thread,
  // wait for the retirement fence, and try again.
  do {
    if (!image->Acquire()) {
      failures++;
      continue;
    }
    // Re-arm the event
    ASSERT_OK(signal_event.signal(ZX_EVENT_SIGNALED, 0));
    {
      fbl::AutoLock l(image->mtx());
      image->ResetFences();
      image->PrepareFences(nullptr, signal_fence->GetReference());
    }
    zx::result<> post_task_result =
        PostTask<kDisplayTaskTargetSize>(*loop.dispatcher(), [image, &retire_count]() {
          fbl::AutoLock l(image->mtx());
          image->StartPresent();
          retire_count++;
          image->StartRetire();
          image->OnRetire();
        });
    ASSERT_OK(post_task_result);

    async::WaitOnce signal_event_wait(signal_event.get(), ZX_EVENT_SIGNALED, /*options=*/0);
    bool signal_event_signaled = false;
    signal_event_wait.Begin(
        loop.dispatcher(),
        [&signal_event_signaled](async_dispatcher_t* dispatcher, async::WaitOnce* wait,
                                 zx_status_t status, const zx_packet_signal_t* signal) {
          signal_event_signaled = true;
        });
    loop.RunUntilIdle();
    EXPECT_TRUE(signal_event_signaled);
  } while (--attempts > 0);
  EXPECT_EQ(0u, failures);
  EXPECT_EQ(kNumIterations, retire_count);
  {
    fbl::AutoLock l(image->mtx());
    image->ResetFences();
  }
  image->EarlyRetire();
}

}  // namespace display
