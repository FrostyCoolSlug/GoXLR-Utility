use goxlr_types::{ChannelName, FaderName};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeviceStatus {
    pub device_type: DeviceType,
    pub usb_device: Option<UsbProductInformation>,
    pub mixer: Option<MixerStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MixerStatus {
    pub fader_a_assignment: ChannelName,
    pub fader_b_assignment: ChannelName,
    pub fader_c_assignment: ChannelName,
    pub fader_d_assignment: ChannelName,
    pub mic_volume: u8,
    pub line_in_volume: u8,
    pub console_volume: u8,
    pub system_volume: u8,
    pub game_volume: u8,
    pub chat_volume: u8,
    pub sample_volume: u8,
    pub music_volume: u8,
    pub headphones_volume: u8,
    pub mic_monitor_volume: u8,
    pub line_out_volume: u8,
}

impl MixerStatus {
    pub fn get_fader_assignment(&self, fader: FaderName) -> ChannelName {
        match fader {
            FaderName::A => self.fader_a_assignment,
            FaderName::B => self.fader_b_assignment,
            FaderName::C => self.fader_c_assignment,
            FaderName::D => self.fader_d_assignment,
        }
    }

    pub fn set_fader_assignment(&mut self, fader: FaderName, channel: ChannelName) {
        match fader {
            FaderName::A => self.fader_a_assignment = channel,
            FaderName::B => self.fader_b_assignment = channel,
            FaderName::C => self.fader_c_assignment = channel,
            FaderName::D => self.fader_d_assignment = channel,
        }
    }

    pub fn get_channel_volume(&self, channel: ChannelName) -> u8 {
        match channel {
            ChannelName::Mic => self.mic_volume,
            ChannelName::LineIn => self.line_in_volume,
            ChannelName::Console => self.console_volume,
            ChannelName::System => self.system_volume,
            ChannelName::Game => self.game_volume,
            ChannelName::Chat => self.chat_volume,
            ChannelName::Sample => self.sample_volume,
            ChannelName::Music => self.music_volume,
            ChannelName::Headphones => self.headphones_volume,
            ChannelName::MicMonitor => self.mic_monitor_volume,
            ChannelName::LineOut => self.line_out_volume,
        }
    }

    pub fn set_channel_volume(&mut self, channel: ChannelName, volume: u8) {
        match channel {
            ChannelName::Mic => self.mic_volume = volume,
            ChannelName::LineIn => self.line_in_volume = volume,
            ChannelName::Console => self.console_volume = volume,
            ChannelName::System => self.system_volume = volume,
            ChannelName::Game => self.game_volume = volume,
            ChannelName::Chat => self.chat_volume = volume,
            ChannelName::Sample => self.sample_volume = volume,
            ChannelName::Music => self.music_volume = volume,
            ChannelName::Headphones => self.headphones_volume = volume,
            ChannelName::MicMonitor => self.mic_monitor_volume = volume,
            ChannelName::LineOut => self.line_out_volume = volume,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsbProductInformation {
    pub manufacturer_name: String,
    pub product_name: String,
    pub version: (u8, u8, u8),
    pub is_claimed: bool,
    pub has_kernel_driver_attached: bool,
    pub bus_number: u8,
    pub address: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DeviceType {
    Unknown,
    Full,
    Mini,
}

impl Default for DeviceType {
    fn default() -> Self {
        DeviceType::Unknown
    }
}
