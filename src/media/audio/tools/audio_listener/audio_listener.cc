// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be found in the LICENSE file.

#include "src/media/audio/tools/audio_listener/audio_listener.h"

#include <fuchsia/media/cpp/fidl.h>
#include <lib/fidl/cpp/enum.h>

#include <cstdio>
#include <iostream>
#include <optional>
#include <ostream>

#include "src/lib/fxl/command_line.h"
#include "src/lib/fxl/strings/string_printf.h"

namespace media {
namespace {

constexpr char kClearEol[] = "\x1b[K";
constexpr char kHideCursor[] = "\x1b[?25l";
constexpr char kShowCursor[] = "\x1b[?25h";

// This should reflect the order of the fuchsia::media::AudioCaptureUsage enum's underlying values.
constexpr std::array<uint8_t, fuchsia::media::CAPTURE_USAGE_COUNT> kAlphaIndexForCaptureUsage = {
    0,  // BACKGROUND
    2,  // FOREGROUND
    3,  // SYSTEM_AGENT
    1,  // COMMUNICATION
};

// This should reflect the order of the fuchsia::media::AudioRenderUsage enum's underlying values.
constexpr std::array<uint8_t, fuchsia::media::RENDER_USAGE_COUNT> kAlphaIndexForRenderUsage = {
    0,  // BACKGROUND
    3,  // MEDIA
    2,  // INTERRUPTION
    4,  // SYSTEM_AGENT
    1,  // COMMUNICATION
};
uint8_t CaptureUsageToAlphaIndex(fuchsia::media::AudioCaptureUsage capture_usage) {
  return kAlphaIndexForCaptureUsage[static_cast<uint8_t>(capture_usage)];
}
uint8_t RenderUsageToAlphaIndex(fuchsia::media::AudioRenderUsage render_usage) {
  return kAlphaIndexForRenderUsage[static_cast<uint8_t>(render_usage)];
}

// We can list these in alphabetical, so that the UI displays in that order.
const std::pair<fuchsia::media::AudioRenderUsage, std::string>
    kRenderUsages[fuchsia::media::RENDER_USAGE_COUNT] = {
        {fuchsia::media::AudioRenderUsage::BACKGROUND, "Backgd"},
        {fuchsia::media::AudioRenderUsage::COMMUNICATION, "Comm  "},
        {fuchsia::media::AudioRenderUsage::INTERRUPTION, "Interr"},
        {fuchsia::media::AudioRenderUsage::MEDIA, "Media "},
        {fuchsia::media::AudioRenderUsage::SYSTEM_AGENT, "SysAgt"},
};
const std::pair<fuchsia::media::AudioCaptureUsage, std::string>
    kCaptureUsages[fuchsia::media::CAPTURE_USAGE_COUNT] = {
        {fuchsia::media::AudioCaptureUsage::BACKGROUND, "Backgd"},
        {fuchsia::media::AudioCaptureUsage::COMMUNICATION, "Comm  "},
        {fuchsia::media::AudioCaptureUsage::FOREGROUND, "Foregd"},
        {fuchsia::media::AudioCaptureUsage::SYSTEM_AGENT, "SysAgt"},
};
constexpr const char* kBlankUsageName = "      ";

}  // namespace

UsageGainListenerImpl::UsageGainListenerImpl(AudioListener* parent, std::string device_str,
                                             fuchsia::media::Usage usage)
    : parent_(parent),
      binding_(this),
      device_str_(std::move(device_str)),
      usage_(std::move(usage)) {
  if (usage_.is_capture_usage()) {
    usage_str_ = "AudioCaptureUsage::" +
                 kCaptureUsages[CaptureUsageToAlphaIndex(usage_.capture_usage())].second;
  } else {
    usage_str_ =
        "AudioRenderUsage::" + kRenderUsages[RenderUsageToAlphaIndex(usage_.render_usage())].second;
  }
}

void UsageGainListenerImpl::OnGainMuteChanged(bool muted, float gain_dbfs,
                                              OnGainMuteChangedCallback callback) {
  // Mute is not currently supported/emitted by the UsageGain server implementation.
  muted_ = muted;
  gain_db_ = gain_dbfs;

  FX_LOGS(DEBUG) << "UsageGainListener('" << device_str_ << "', " << usage_str_
                 << ")::OnGainMuteChanged(" << (muted_ ? "muted" : "unmuted") << ", " << gain_db_
                 << ")";

  callback();
  parent()->RefreshDisplay();
}

UsageWatcherImpl::UsageWatcherImpl(AudioListener* parent, fuchsia::media::Usage usage)
    : parent_(parent), binding_(this), usage_(std::move(usage)) {
  if (usage_.is_capture_usage()) {
    usage_str_ = "AudioCaptureUsage::" +
                 kCaptureUsages[CaptureUsageToAlphaIndex(usage_.capture_usage())].second;
  } else {
    usage_str_ =
        "AudioRenderUsage::" + kRenderUsages[RenderUsageToAlphaIndex(usage_.render_usage())].second;
  }
}

void UsageWatcherImpl::OnStateChanged(fuchsia::media::Usage usage,
                                      fuchsia::media::UsageState usage_state,
                                      OnStateChangedCallback callback) {
  if (usage_.is_capture_usage()) {
    if (!usage.is_capture_usage()) {
      FX_LOGS(ERROR) << "Usage Mismatch: usage_ is_cap(" << usage_.is_capture_usage() << ")";
    }
    if (usage_.capture_usage() != usage.capture_usage()) {
      FX_LOGS(ERROR) << "Usage Mismatch: usage_ " << static_cast<uint32_t>(usage_.capture_usage())
                     << ", usage " << static_cast<uint32_t>(usage.capture_usage());
    }
  } else {
    if (!usage.is_render_usage()) {
      FX_LOGS(ERROR) << "Usage Mismatch: usage_ is_ren(" << usage_.is_render_usage() << ")";
    }
    if (usage_.render_usage() != usage.render_usage()) {
      FX_LOGS(ERROR) << "Usage Mismatch: usage_ " << static_cast<uint32_t>(usage_.render_usage())
                     << ", usage " << static_cast<uint32_t>(usage.render_usage());
    }
  }

  std::string usage_state_str;
  if (usage_state.is_unadjusted()) {
    usage_state_str = "Unadjusted";
  } else {
    usage_state_str = usage_state.is_ducked() ? "Ducked" : "Muted";
  }
  FX_LOGS(DEBUG) << "UsageWatcher::OnStateChanged(" << usage_str_ << ", " << usage_state_str << ")";

  usage_state_ = std::move(usage_state);

  callback();
  parent()->RefreshDisplay();
}

std::string UsageWatcherImpl::usage_state_str() const {
  if (usage_state_.is_unadjusted()) {
    return "norm";
  }
  return usage_state_.is_ducked() ? "DUCK" : "MUTE";
}

//////
AudioListener::AudioListener(int argc, const char** argv, fit::closure quit_callback)
    : component_context_(sys::ComponentContext::CreateAndServeOutgoingDirectory()),
      quit_callback_(std::move(quit_callback)) {
  activity_reporter_ = component_context_->svc()->Connect<fuchsia::media::ActivityReporter>();
  activity_reporter_.set_error_handler([this](zx_status_t status) {
    FX_PLOGS(ERROR, status) << "Client connection to fuchsia.media.ActivityReporter failed";
    quit_callback_();
  });

  usage_reporter_ = component_context_->svc()->Connect<fuchsia::media::UsageReporter>();
  usage_reporter_.set_error_handler([this](zx_status_t status) {
    FX_PLOGS(ERROR, status) << "Client connection to fuchsia.media.UsageReporter failed";
    quit_callback_();
  });

  audio_core_ = component_context_->svc()->Connect<fuchsia::media::AudioCore>();
  audio_core_.set_error_handler([this](zx_status_t status) {
    FX_PLOGS(ERROR, status) << "Client connection to fuchsia.media.AudioCore failed";
    quit_callback_();
  });

  usage_gain_reporter_ = component_context_->svc()->Connect<fuchsia::media::UsageGainReporter>();
  usage_gain_reporter_.set_error_handler([this](zx_status_t status) {
    FX_PLOGS(ERROR, status) << "Client connection to fuchsia.media.UsageGainReporter failed";
    quit_callback_();
  });
}

void AudioListener::Run() {
  // Get the party started by watching for usage activity.
  WatchRenderActivity();
  WatchCaptureActivity();
  WatchUsageStates();
  WatchUsageVolumes();
  WatchUsageGains();

  DisplayHeader();
  setbuf(stdin, nullptr);
  RefreshDisplay();
  WaitForKeystroke();
}

void AudioListener::WatchRenderActivity() {
  activity_reporter_->WatchRenderActivity(
      [this](const std::vector<fuchsia::media::AudioRenderUsage>& render_usages) {
        OnRenderActivity(render_usages);
      });
}

void AudioListener::WatchCaptureActivity() {
  activity_reporter_->WatchCaptureActivity(
      [this](const std::vector<fuchsia::media::AudioCaptureUsage>& capture_usages) {
        OnCaptureActivity(capture_usages);
      });
}

void AudioListener::OnRenderActivity(
    const std::vector<fuchsia::media::AudioRenderUsage>& render_usages) {
  // First clear the existing activity
  for (const auto& render_usage_watcher : render_usage_watchers_) {
    render_usage_watcher->set_active(false);
  }
  // Then set the usages 'active' that are contained in the render_usages vector
  for (auto render_usage : render_usages) {
    render_usage_watchers_[RenderUsageToAlphaIndex(render_usage)]->set_active(true);
  }
  RefreshDisplay();
  WatchRenderActivity();
}

void AudioListener::OnCaptureActivity(
    const std::vector<fuchsia::media::AudioCaptureUsage>& capture_usages) {
  // First clear the existing activity
  for (const auto& capture_usage_watcher : capture_usage_watchers_) {
    capture_usage_watcher->set_active(false);
  }
  // Then set the usages 'active' that are contained in the capture_usages vector
  for (auto capture_usage : capture_usages) {
    capture_usage_watchers_[CaptureUsageToAlphaIndex(capture_usage)]->set_active(true);
  }
  RefreshDisplay();
  WatchCaptureActivity();
}

void AudioListener::WatchUsageStates() {
  for (auto r_idx = 0u; r_idx < fuchsia::media::RENDER_USAGE_COUNT; ++r_idx) {
    fuchsia::media::Usage usage =
        fuchsia::media::Usage::WithRenderUsage(fidl::Clone(kRenderUsages[r_idx].first));
    render_usage_watchers_[r_idx] = std::make_unique<UsageWatcherImpl>(
        this, fuchsia::media::Usage::WithRenderUsage(fidl::Clone(kRenderUsages[r_idx].first)));

    usage_reporter_->Watch(std::move(usage), render_usage_watchers_[r_idx]->NewBinding());
    render_usage_watchers_[r_idx]->binding().set_error_handler([this, r_idx](zx_status_t status) {
      FX_PLOGS(ERROR, status) << "Client connection to fuchsia.media.UsageWatcher failed for r_idx "
                              << r_idx;
      quit_callback_();
    });
  }

  for (auto c_idx = 0u; c_idx < fuchsia::media::CAPTURE_USAGE_COUNT; ++c_idx) {
    fuchsia::media::Usage usage =
        fuchsia::media::Usage::WithCaptureUsage(fidl::Clone(kCaptureUsages[c_idx].first));
    capture_usage_watchers_[c_idx] = std::make_unique<UsageWatcherImpl>(
        this, fuchsia::media::Usage::WithCaptureUsage(fidl::Clone(kCaptureUsages[c_idx].first)));

    usage_reporter_->Watch(std::move(usage), capture_usage_watchers_[c_idx]->NewBinding());
    capture_usage_watchers_[c_idx]->binding().set_error_handler([this, c_idx](zx_status_t status) {
      FX_PLOGS(ERROR, status) << "Client connection to fuchsia.media.UsageWatcher failed for c_idx "
                              << c_idx;
      quit_callback_();
    });
  }
}

void AudioListener::WatchUsageVolumes() {
  for (auto r_idx = 0u; r_idx < fuchsia::media::RENDER_USAGE_COUNT; ++r_idx) {
    audio_core_->BindUsageVolumeControl(
        fuchsia::media::Usage::WithRenderUsage(fidl::Clone(kRenderUsages[r_idx].first)),
        render_usage_volume_ctls_[r_idx].NewRequest());

    render_usage_volume_ctls_[r_idx].set_error_handler([this, r_idx](zx_status_t status) {
      FX_PLOGS(ERROR, status)
          << "Client connection to fuchsia.media.VolumeControl failed for r_idx " << r_idx;
      quit_callback_();
    });

    render_usage_volume_ctls_[r_idx].events().OnVolumeMuteChanged = [this, r_idx](float volume,
                                                                                  bool muted) {
      render_usage_volumes_[r_idx] = volume;
      render_usage_mutes_[r_idx] = muted;
      RefreshDisplay();
    };
  }
}

void AudioListener::WatchUsageGains() {
  const std::string output_device_str = "01000000000000000000000000000000";
  const std::string input_device_str = "03000000000000000000000000000000";

  for (auto r_idx = 0u; r_idx < fuchsia::media::RENDER_USAGE_COUNT; ++r_idx) {
    fuchsia::media::Usage usage =
        fuchsia::media::Usage::WithRenderUsage(fidl::Clone(kRenderUsages[r_idx].first));
    render_usage_gain_listeners_[r_idx] = std::make_unique<UsageGainListenerImpl>(
        this, output_device_str,
        fuchsia::media::Usage::WithRenderUsage(fidl::Clone(kRenderUsages[r_idx].first)));

    usage_gain_reporter_->RegisterListener(output_device_str, std::move(usage),
                                           render_usage_gain_listeners_[r_idx]->NewBinding());
    render_usage_gain_listeners_[r_idx]->binding().set_error_handler(
        [this, r_idx](zx_status_t status) {
          FX_PLOGS(ERROR, status)
              << "Client connection to fuchsia.media.UsageGainListener failed for r_idx " << r_idx;
          quit_callback_();
        });
  }

  for (auto c_idx = 0u; c_idx < fuchsia::media::CAPTURE_USAGE_COUNT; ++c_idx) {
    fuchsia::media::Usage usage =
        fuchsia::media::Usage::WithCaptureUsage(fidl::Clone(kCaptureUsages[c_idx].first));
    capture_usage_gain_listeners_[c_idx] = std::make_unique<UsageGainListenerImpl>(
        this, input_device_str,
        fuchsia::media::Usage::WithCaptureUsage(fidl::Clone(kCaptureUsages[c_idx].first)));

    usage_gain_reporter_->RegisterListener(input_device_str, std::move(usage),
                                           capture_usage_gain_listeners_[c_idx]->NewBinding());
    capture_usage_gain_listeners_[c_idx]->binding().set_error_handler(
        [this, c_idx](zx_status_t status) {
          FX_PLOGS(ERROR, status)
              << "Client connection to fuchsia.media.UsageGainListener failed for c_idx " << c_idx;
          quit_callback_();
        });
  }
}

// static
void AudioListener::DisplayHeader() {
  std::cout << "\n         |                   Render usages                  ||             "
               "Capture usages              |";
  std::cout << kHideCursor << '\n';
}

void AudioListener::DisplayUsageActivity() {
  std::cout << "Active: |    ";
  for (uint8_t r_idx = 0; r_idx < fuchsia::media::RENDER_USAGE_COUNT; ++r_idx) {
    std::cout << (render_usage_watchers_[r_idx]->active() ? kRenderUsages[r_idx].second
                                                          : kBlankUsageName);
    std::cout << "   ";
  }
  std::cout << " ||    ";
  for (uint8_t c_idx = 0; c_idx < fuchsia::media::CAPTURE_USAGE_COUNT; ++c_idx) {
    std::cout << (capture_usage_watchers_[c_idx]->active() ? kCaptureUsages[c_idx].second
                                                           : kBlankUsageName);
    std::cout << "   ";
  }
}

void AudioListener::DisplayUsageStates() {
  std::cout << "States: |    ";
  for (uint8_t r_idx = 0; r_idx < fuchsia::media::RENDER_USAGE_COUNT; ++r_idx) {
    std::cout << fxl::StringPrintf("%c %s   ", kRenderUsages[r_idx].second[0],
                                   render_usage_watchers_[r_idx]->usage_state_str().c_str());
  }
  std::cout << " ||    ";
  for (uint8_t c_idx = 0; c_idx < fuchsia::media::CAPTURE_USAGE_COUNT; ++c_idx) {
    std::cout << fxl::StringPrintf("%c %s   ", kCaptureUsages[c_idx].second[0],
                                   capture_usage_watchers_[c_idx]->usage_state_str().c_str());
  }
}

void AudioListener::DisplayUsageVolumes() {
  std::cout << "Volume: |    ";
  for (uint8_t r_idx = 0; r_idx < fuchsia::media::RENDER_USAGE_COUNT; ++r_idx) {
    std::cout << fxl::StringPrintf("%c %4.2f   ", kRenderUsages[r_idx].second[0],
                                   render_usage_volumes_[r_idx]);
  }
  std::cout << " ||                                        ";
}

void AudioListener::DisplayUsageGains() {
  std::cout << "GainDb: |    ";
  for (uint8_t r_idx = 0; r_idx < fuchsia::media::RENDER_USAGE_COUNT; ++r_idx) {
    std::cout << fxl::StringPrintf("%c%6.1f  ", kRenderUsages[r_idx].second[0],
                                   render_usage_gain_listeners_[r_idx]->gain_db());
  }
  std::cout << " ||    ";
  for (uint8_t c_idx = 0; c_idx < fuchsia::media::CAPTURE_USAGE_COUNT; ++c_idx) {
    std::cout << fxl::StringPrintf("%c%6.1f  ", kCaptureUsages[c_idx].second[0],
                                   capture_usage_gain_listeners_[c_idx]->gain_db());
  }
}

void AudioListener::RefreshDisplay() {
  std::cout << "\r ";
  switch (display_mode_) {
    case DisplayMode::UsageActive:
      DisplayUsageActivity();
      break;
    case DisplayMode::UsageState:
      DisplayUsageStates();
      break;
    case DisplayMode::UsageVolume:
      DisplayUsageVolumes();
      break;
    case DisplayMode::UsageGain:
      DisplayUsageGains();
      break;
  }
  std::cout << " |" << kClearEol << std::flush;
}

// Calls |HandleKeystroke| on the message loop when console input is ready.
void AudioListener::WaitForKeystroke() {
  fd_waiter_.Wait([this](zx_status_t status, uint32_t events) { HandleKeystroke(); }, 0, POLLIN);
}

// Handles a keystroke, possibly calling |WaitForKeystroke| to wait for the next one.
void AudioListener::HandleKeystroke() {
  int c = esc_decoder_.Decode(getc(stdin));

  switch (c) {
    case EscapeDecoder::kUpArrow:
    case '1':
      display_mode_ = DisplayMode::UsageActive;
      break;
    case EscapeDecoder::kLeftArrow:
    case '2':
      display_mode_ = DisplayMode::UsageState;
      break;
    case EscapeDecoder::kDownArrow:
    case '3':
      display_mode_ = DisplayMode::UsageVolume;
      break;
    case EscapeDecoder::kRightArrow:
    case '4':
      display_mode_ = DisplayMode::UsageGain;
      break;
    case '\n':
    case '\r':
    case 'q':
    case 'Q':
      quit_callback_();
      std::cout << kShowCursor << "\n\n";
      return;
    default:
      break;
  }
  RefreshDisplay();

  WaitForKeystroke();
}

}  // namespace media

namespace {

void DisplayUsage(const std::string& name, std::optional<std::string> error_str = std::nullopt) {
  printf("\n");
  if (error_str.has_value()) {
    printf("%s\n\n", error_str->c_str());
  }

  printf("Usage: %s [--help | --?]\n\n", name.c_str());

  printf("This tool displays per-usage metadata. The following information is updated in\n");
  printf("in real-time, for all render and capture usages:\n\n");

  printf("  - Activity (whether the usage is active), per fuchsia.media.ActivityReporter\n");
  printf("  - State (Normal/Ducked/Muted), per fuchsia.media.UsageWatcher\n");
  printf("  - Volume (0.0 - 1.0), from fuchsia.media.AudioCore/BindUsageVolumeControl\n");
  printf("  - Gain (dB), per fuchsia.media.UsageGainListener\n\n");

  printf("To switch between Activity | State | Volume | Gain display modes, press arrow keys\n");
  printf("(up | left | down | right for Activity | State | Volume | Gain respectively), or\n");
  printf("numerical keys 1-4 (handy when arrow keys are unavailable).\n\n");

  printf("In Activity mode, for every usage a six-letter abbreviation is displayed IFF it is\n");
  printf("active: Backgd, Comm, Foregd, Interr, Media, SysAgt.\n\n");

  printf("In State, Volume and Gain modes, the first letter of each usage is shown alongside\n");
  printf("that usage's information.\n\n");

  printf(
      "Render Usages include:  "
      "Background, Communication, Interruption, Media, SystemAgent\n");
  printf("Capture Usages include: Background, Communication, Foreground, SystemAgent\n\n");

  printf("To quit the %s tool, press Q or [Enter].\n\n", name.c_str());
}

std::optional<int> HandleCommandLine(const fxl::CommandLine& command_line) {
  const auto& argv0 = command_line.argv0();
  if (!command_line.positional_args().empty()) {
    DisplayUsage(argv0, fxl::StringPrintf("The %s tool does not accept positional arguments.",
                                          argv0.c_str()));
    return -1;
  }
  if (command_line.options().size() > 1) {
    DisplayUsage(argv0, "Too many cmdline options.");
    return -1;
  }

  if (command_line.HasOption("help") || command_line.HasOption("?")) {
    DisplayUsage(argv0);
    return 0;
  }

  if (!command_line.options().empty()) {
    DisplayUsage(argv0, "Unknown cmdline option.");
    return -1;
  }

  return std::nullopt;
}

}  // namespace

int main(int argc, const char** argv) {
  auto command_line = fxl::CommandLineFromArgcArgv(argc, argv);
  fuchsia_logging::LogSettingsBuilder builder;
  builder.WithTags({command_line.argv0()}).BuildAndInitialize();
  if (auto has_cmdline_args = HandleCommandLine(command_line); has_cmdline_args.has_value()) {
    return *has_cmdline_args;
  }

  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);

  media::AudioListener audio_listener(
      argc, argv, [&loop]() { async::PostTask(loop.dispatcher(), [&loop]() { loop.Quit(); }); });
  audio_listener.Run();

  loop.Run();
  return 0;
}
