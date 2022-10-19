use std::cmp::Ordering;
use std::collections::HashMap;
use std::fs::{remove_file, File};
use std::io::{Cursor, Read, Seek};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use enum_map::EnumMap;
use enumset::EnumSet;
use log::{debug, error, warn};
use strum::EnumCount;
use strum::IntoEnumIterator;

use crate::audio::AudioFile;
use goxlr_ipc::{
    ActiveEffects, ButtonLighting, CoughButton, Echo, Effects, FaderLighting, Gender, HardTune,
    Lighting, Megaphone, OneColour, Pitch, Reverb, Robot, Sampler, SamplerButton, SamplerLighting,
    ThreeColours, TwoColours,
};
use goxlr_profile_loader::components::colours::{
    Colour, ColourDisplay, ColourMap, ColourOffStyle, ColourState,
};
use goxlr_profile_loader::components::echo::{EchoEncoder, EchoStyle};
use goxlr_profile_loader::components::gender::{GenderEncoder, GenderStyle};
use goxlr_profile_loader::components::hardtune::{HardTuneEffect, HardTuneSource, HardTuneStyle};
use goxlr_profile_loader::components::megaphone::{MegaphoneEffect, MegaphoneStyle};
use goxlr_profile_loader::components::mixer::{FullChannelList, InputChannels, OutputChannels};
use goxlr_profile_loader::components::mute::{MuteButton, MuteFunction};
use goxlr_profile_loader::components::mute_chat::{CoughToggle, MuteChat};
use goxlr_profile_loader::components::pitch::{PitchEncoder, PitchStyle};
use goxlr_profile_loader::components::reverb::{ReverbEncoder, ReverbStyle};
use goxlr_profile_loader::components::robot::{RobotEffect, RobotStyle};
use goxlr_profile_loader::components::sample::{PlayOrder, PlaybackMode, SampleBank, Track};
use goxlr_profile_loader::components::simple::SimpleElements;
use goxlr_profile_loader::profile::{Profile, ProfileSettings};
use goxlr_profile_loader::SampleButtons::{BottomLeft, BottomRight, Clear, TopLeft, TopRight};
use goxlr_profile_loader::{Faders, Preset, SampleButtons};
use goxlr_types::{
    ButtonColourGroups, ButtonColourOffStyle as BasicColourOffStyle, ButtonColourTargets,
    ChannelName, EffectBankPresets, EncoderColourTargets, FaderDisplayStyle as BasicColourDisplay,
    FaderName, InputDevice, MuteFunction as BasicMuteFunction, OutputDevice, SamplePlayOrder,
    SamplePlaybackMode, SamplerColourTargets, SimpleColourTargets, VersionNumber,
};
use goxlr_usb::buttonstate::{ButtonStates, Buttons};
use goxlr_usb::colouring::ColourTargets;

use crate::files::{can_create_new_file, create_path};

pub const DEFAULT_PROFILE_NAME: &str = "DEFAULT";
const DEFAULT_PROFILE: &[u8] = include_bytes!("../profiles/DEFAULT.goxlr");

#[derive(Debug)]
pub struct ProfileAdapter {
    name: String,
    profile: Profile,
}

impl ProfileAdapter {
    pub fn from_named_or_default(name: Option<String>, directories: Vec<&Path>) -> Self {
        if let Some(name) = name {
            match ProfileAdapter::from_named(name.clone(), directories) {
                Ok(result) => return result,
                Err(error) => error!("Couldn't load profile {}: {}", name, error),
            }
        }

        ProfileAdapter::default()
    }

    pub fn from_named(name: String, directories: Vec<&Path>) -> Result<Self> {
        let mut dir_list = "".to_string();

        // Loop through the provided directories, and try to find the profile..
        for directory in directories {
            let path = directory.join(format!("{}.goxlr", name));

            if path.is_file() {
                debug!("Loading Profile From {}", path.to_string_lossy());
                let file = File::open(path).context("Couldn't open profile for reading")?;
                return ProfileAdapter::from_reader(name, file);
            }
            dir_list = format!("{}, {}", dir_list, directory.to_string_lossy());
        }

        if name == DEFAULT_PROFILE_NAME {
            debug!("Loading Embedded Default Profile..");
            return Ok(ProfileAdapter::default());
        }

        Err(anyhow!(
            "Profile {} does not exist inside {:?}",
            name,
            dir_list
        ))
    }

    pub fn default() -> Self {
        ProfileAdapter::from_reader(
            DEFAULT_PROFILE_NAME.to_string(),
            Cursor::new(DEFAULT_PROFILE),
        )
        .expect("Default profile isn't available")
    }

    pub fn from_reader<R: Read + Seek>(name: String, reader: R) -> Result<Self> {
        let profile = Profile::load(reader)?;
        Ok(Self { name, profile })
    }

    pub fn can_create_new_file(name: String, directory: &Path) -> Result<()> {
        let path = directory.join(format!("{}.goxlr", name));
        can_create_new_file(path)
    }

    pub fn write_profile(&mut self, name: String, directory: &Path, overwrite: bool) -> Result<()> {
        let path = directory.join(format!("{}.goxlr", name));
        create_path(directory)?;

        if !overwrite && path.is_file() {
            return Err(anyhow!("Profile exists, will not overwrite"));
        }

        self.profile.save(path)?;

        // Keep our names in sync (in case it was changed)
        if name != self.name() {
            dbg!("Changing Profile Name: {} -> {}", self.name(), name.clone());
            self.name = name;
        }

        Ok(())
    }

    pub fn write_preset(&mut self, name: String, directory: &Path) -> Result<()> {
        let path = directory.join(format!("{}.preset", name));
        create_path(directory)?;
        self.profile.save_preset(path)?;
        Ok(())
    }

    pub fn delete_profile(&mut self, name: String, directory: &Path) -> Result<()> {
        let path = directory.join(format!("{}.goxlr", name));
        if path.is_file() {
            remove_file(path)?;
        }
        Ok(())
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn load_colour_profile(&mut self, new_profile: ProfileAdapter) {
        for colour in ColourTargets::iter() {
            let our_map = get_profile_colour_map_mut(self.profile.settings_mut(), colour);
            let new_map = get_profile_colour_map(new_profile.profile.settings(), colour);

            our_map.replace(new_map);
        }
    }

    pub fn load_preset(&mut self, name: String, directories: Vec<&Path>) -> Result<()> {
        let mut dir_list = "".to_string();

        // Loop through the provided directories, and try to find the preset..
        for directory in directories {
            let path = directory.join(format!("{}.preset", name));

            if path.is_file() {
                debug!("Loading Preset From {}", path.to_string_lossy());
                let file = File::open(path).context("Couldn't open preset for reading")?;

                self.profile.settings_mut().load_preset(file)?;
                return Ok(());
            }
            dir_list = format!("{}, {}", dir_list, directory.to_string_lossy());
        }

        Err(anyhow!(
            "Preset {} does not exist inside {:?}",
            name,
            dir_list
        ))
    }

    pub fn create_router(&self) -> [EnumSet<OutputDevice>; InputDevice::COUNT] {
        let mut router = [EnumSet::empty(); InputDevice::COUNT];

        for (input, potential_outputs) in self.profile.settings().mixer().mixer_table().iter() {
            let mut outputs = EnumSet::empty();

            for (channel, volume) in potential_outputs.iter() {
                if *volume > 0 {
                    outputs.insert(profile_to_standard_output(channel));
                }
            }

            router[profile_to_standard_input(input) as usize] = outputs;
        }
        router
    }

    // This is similar to above, but provides a slightly 'nicer' true / false for lookups, which
    // maps slightly better when converting to something like JSON, this may fully replace the above
    // but for now will sit along side
    pub fn create_router_table(&self) -> [[bool; OutputDevice::COUNT]; InputDevice::COUNT] {
        let mut router = [[false; OutputDevice::COUNT]; InputDevice::COUNT];

        for (input, potential_outputs) in self.profile.settings().mixer().mixer_table().iter() {
            for (channel, volume) in potential_outputs.iter() {
                if *volume > 0 {
                    router[profile_to_standard_input(input) as usize]
                        [profile_to_standard_output(channel) as usize] = true;
                }
            }
        }
        router
    }

    pub fn get_router(&self, input: InputDevice) -> EnumMap<OutputDevice, bool> {
        let mut map: EnumMap<OutputDevice, bool> = EnumMap::default();

        // Get the mixer table
        let mixer =
            &self.profile.settings().mixer().mixer_table()[standard_input_to_profile(input)];
        for (channel, volume) in mixer.iter() {
            map[profile_to_standard_output(channel)] = *volume > 0;
        }

        map
    }

    pub fn set_routing(&mut self, input: InputDevice, output: OutputDevice, enabled: bool) {
        let input = standard_input_to_profile(input);
        let output = standard_output_to_profile(output);

        let mut value = 8192;
        if !enabled {
            value = 0;
        }

        let table = self.profile.settings_mut().mixer_mut().mixer_table_mut();
        table[input][output] = value;
    }

    pub fn get_fader_assignment(&self, fader: FaderName) -> ChannelName {
        let fader = self
            .profile
            .settings()
            .fader(standard_to_profile_fader(fader));
        profile_to_standard_channel(fader.channel())
    }

    pub fn set_fader_assignment(&mut self, fader: FaderName, channel: ChannelName) {
        self.profile
            .settings_mut()
            .fader_mut(standard_to_profile_fader(fader))
            .set_channel(standard_to_profile_channel(channel));
    }

    pub fn switch_fader_assignment(&mut self, fader_one: FaderName, fader_two: FaderName) {
        // TODO: Scribble?
        self.profile.settings_mut().faders().swap(
            standard_to_profile_fader(fader_one),
            standard_to_profile_fader(fader_two),
        );
        self.profile.settings_mut().mute_buttons().swap(
            standard_to_profile_fader(fader_one),
            standard_to_profile_fader(fader_two),
        );
    }

    pub fn set_fader_display(
        &mut self,
        fader: FaderName,
        display: BasicColourDisplay,
    ) -> Result<()> {
        let colours = self
            .profile
            .settings_mut()
            .fader_mut(standard_to_profile_fader(fader))
            .colour_map_mut();
        colours.set_fader_display(standard_to_profile_fader_display(display))
    }

    // We have a return type here, as there's string parsing involved..
    pub fn set_fader_colours(
        &mut self,
        fader: FaderName,
        top: String,
        bottom: String,
    ) -> Result<()> {
        let colours = self
            .profile
            .settings_mut()
            .fader_mut(standard_to_profile_fader(fader))
            .colour_map_mut();
        colours.set_colour(0, Colour::fromrgb(top.as_str())?)?;
        colours.set_colour(1, Colour::fromrgb(bottom.as_str())?)?;
        Ok(())
    }

    pub fn get_channel_volume(&self, channel: ChannelName) -> u8 {
        self.profile
            .settings()
            .mixer()
            .channel_volume(standard_to_profile_channel(channel))
    }

    pub fn get_volumes(&self) -> [u8; ChannelName::COUNT] {
        let mut volumes = [255; ChannelName::COUNT];
        for channel in ChannelName::iter() {
            volumes[channel as usize] = self.get_channel_volume(channel);
        }

        volumes
    }

    pub fn set_channel_volume(&mut self, channel: ChannelName, volume: u8) -> Result<()> {
        self.profile
            .settings_mut()
            .mixer_mut()
            .set_channel_volume(standard_to_profile_channel(channel), volume)
    }

    pub fn get_colour_map(&self, use_format_1_3_40: bool) -> [u8; 520] {
        let mut colour_array = [0; 520];

        for colour in ColourTargets::iter() {
            let colour_map = get_profile_colour_map(self.profile.settings(), colour);

            for i in 0..colour.get_colour_count() {
                let position = colour.position(i, use_format_1_3_40);

                // Ok, previously this was based on 'is_blank_when_dimmed', but turns out I misinterpreted
                // what was going on there, if a sample button has no samples assigned to it, it'll go
                // dark, so we need to check for that here.
                match colour {
                    ColourTargets::SamplerBottomLeft
                    | ColourTargets::SamplerBottomRight
                    | ColourTargets::SamplerTopLeft
                    | ColourTargets::SamplerTopRight => {
                        colour_array[position..position + 4]
                            .copy_from_slice(&self.get_sampler_lighting(colour, i));
                    }
                    _ => {
                        // Update the correct 4 bytes in the map..
                        colour_array[position..position + 4]
                            .copy_from_slice(&colour_map.colour(i).to_reverse_bytes());
                    }
                }
            }
        }

        colour_array
    }

    fn get_sampler_lighting(&self, target: ColourTargets, index: u8) -> [u8; 4] {
        match target {
            ColourTargets::SamplerBottomLeft => self.get_colour_array(target, BottomLeft, index),
            ColourTargets::SamplerBottomRight => self.get_colour_array(target, BottomRight, index),
            ColourTargets::SamplerTopLeft => self.get_colour_array(target, TopLeft, index),
            ColourTargets::SamplerTopRight => self.get_colour_array(target, TopRight, index),

            // Honestly, we should never reach this, return nothing.
            _ => [00, 00, 00, 00],
        }
    }

    fn get_colour_array(&self, target: ColourTargets, button: SampleButtons, index: u8) -> [u8; 4] {
        if self.current_sample_bank_has_samples(profile_to_standard_sample_button(button)) {
            return get_profile_colour_map(self.profile.settings(), target)
                .colour(index)
                .to_reverse_bytes();
        }

        // Ok, if we don't have a sample, we need to switch colours 0 and 1..
        let new_index = if index == 0 {
            1
        } else if index == 1 {
            0
        } else {
            index
        };

        // For buttons without samples, we simply use colour1 (this gets configured when
        // loading the bank)..
        return get_profile_colour_map(self.profile.settings(), target)
            .colour_or_default(new_index)
            .to_reverse_bytes();
    }

    fn get_button_colour_map(&self, button: Buttons) -> &ColourMap {
        get_colour_map_from_button(self.profile.settings(), button)
    }

    pub fn get_lighting_ipc(&self, is_device_mini: bool) -> Lighting {
        let mut fader_map: HashMap<FaderName, FaderLighting> = HashMap::new();
        for fader in FaderName::iter() {
            let colour_target = map_fader_to_colour_target(fader);
            let colour_map = get_profile_colour_map(self.profile.settings(), colour_target);

            // Set TwoColour as the default..
            let mut fader_style = BasicColourDisplay::TwoColour;
            if let Some(style) = colour_map.fader_display() {
                fader_style = profile_to_standard_fader_display(*style);
            }

            // Insert the colours, pulling a default (black) if not found
            fader_map.insert(
                fader,
                FaderLighting {
                    style: fader_style,
                    colours: TwoColours {
                        colour_one: colour_map.colour_or_default(0).to_rgb(),
                        colour_two: colour_map.colour_or_default(1).to_rgb(),
                    },
                },
            );
        }

        let mut button_map: HashMap<ButtonColourTargets, ButtonLighting> = HashMap::new();

        let buttons = if is_device_mini {
            get_mini_colour_targets()
        } else {
            ButtonColourTargets::iter().collect()
        };

        let mut ignore_buttons = vec![];
        ignore_buttons.append(&mut get_sampler_colour_targets());
        ignore_buttons.append(&mut get_sampler_selector_colour_targets());

        for button in buttons {
            if ignore_buttons.contains(&button) {
                continue;
            }

            let colour_target = standard_to_colour_target(button);
            let colour_map = get_profile_colour_map(self.profile.settings(), colour_target);

            let off_style = profile_to_standard_colour_off_style(*colour_map.get_off_style());

            // TODO: Sampler Buttons are technically three colours!

            button_map.insert(
                button,
                ButtonLighting {
                    off_style,
                    colours: TwoColours {
                        colour_one: colour_map.colour_or_default(0).to_rgb(),
                        colour_two: colour_map.colour_or_default(1).to_rgb(),
                    },
                },
            );
        }

        let mut simple_map: HashMap<SimpleColourTargets, OneColour> = HashMap::new();
        let ignore_mini_colours = get_scribble_colour_targets();
        for colour in SimpleColourTargets::iter() {
            if is_device_mini && ignore_mini_colours.contains(&colour) {
                continue;
            }

            if colour == SimpleColourTargets::Global {
                // Global is never passed to the GoXLR Verbatim, it's instead a 'wrapper' that
                // changes all the GoXLRs colours at once.
                let colour_map = self
                    .profile
                    .settings()
                    .simple_element(SimpleElements::GlobalColour)
                    .colour_map();

                simple_map.insert(
                    colour,
                    OneColour {
                        colour_one: colour_map.colour_or_default(0).to_rgb(),
                    },
                );
                continue;
            }

            let colour_target = standard_to_profile_simple_colour(colour);
            let colour_map = get_profile_colour_map(self.profile.settings(), colour_target);

            simple_map.insert(
                colour,
                OneColour {
                    colour_one: colour_map.colour_or_default(0).to_rgb(),
                },
            );
        }

        // Encoder Settings..
        let mut encoder_map: HashMap<EncoderColourTargets, ThreeColours> = HashMap::new();
        for colour in EncoderColourTargets::iter() {
            if is_device_mini {
                continue;
            }

            let colour_target = standard_to_profile_encoder_colour(colour);
            let colour_map = get_profile_colour_map(self.profile.settings(), colour_target);

            encoder_map.insert(
                colour,
                ThreeColours {
                    colour_one: colour_map.colour_or_default(0).to_rgb(),
                    colour_two: colour_map.colour_or_default(1).to_rgb(),
                    colour_three: colour_map.colour_or_default(2).to_rgb(),
                },
            );
        }

        let mut sampler_map = HashMap::new();
        for colour in SamplerColourTargets::iter() {
            if is_device_mini {
                continue;
            }

            let colour_target = standard_to_sample_colour(colour);
            let colour_map = get_profile_colour_map(self.profile.settings(), colour_target);
            let off_style = profile_to_standard_colour_off_style(*colour_map.get_off_style());

            sampler_map.insert(
                colour,
                SamplerLighting {
                    off_style,
                    colours: ThreeColours {
                        colour_one: colour_map.colour_or_default(0).to_rgb(),
                        colour_two: colour_map.colour_or_default(1).to_rgb(),
                        colour_three: colour_map.colour_or_default(2).to_rgb(),
                    },
                },
            );
        }

        Lighting {
            faders: fader_map,
            buttons: button_map,
            simple: simple_map,
            sampler: sampler_map,
            encoders: encoder_map,
        }
    }

    pub fn get_effects_ipc(&self, is_device_mini: bool) -> Option<Effects> {
        // There's no point returning effects for a Mini, it doesn't support them!
        if is_device_mini {
            return None;
        }

        // Current Preset
        let active_preset =
            profile_to_standard_preset(self.profile.settings().context().selected_effects());
        let mut preset_names = HashMap::new();
        for preset in EffectBankPresets::iter() {
            preset_names.insert(
                preset,
                self.profile
                    .settings()
                    .effects(standard_to_profile_preset(preset))
                    .name()
                    .to_string(),
            );
        }

        let reverb = Reverb {
            style: profile_to_standard_reverb_style(self.get_active_reverb_profile().style()),
            amount: self.get_active_reverb_profile().get_percentage_amount(),
            decay: self.get_active_reverb_profile().get_decay_millis(),
            early_level: self.get_active_reverb_profile().early_level(),
            tail_level: self.get_active_reverb_profile().tail_level(),
            pre_delay: self.get_active_reverb_profile().predelay(),
            lo_colour: self.get_active_reverb_profile().low_color(),
            hi_colour: self.get_active_reverb_profile().high_color(),
            hi_factor: self.get_active_reverb_profile().hifactor(),
            diffuse: self.get_active_reverb_profile().diffuse(),
            mod_speed: self.get_active_reverb_profile().mod_speed(),
            mod_depth: self.get_active_reverb_profile().mod_depth(),
        };

        let echo = Echo {
            style: profile_to_standard_echo_style(self.get_active_echo_profile().style()),
            amount: self.get_active_echo_profile().get_percentage_amount(),
            feedback: self.get_active_echo_profile().feedback_control(),
            tempo: self.get_active_echo_profile().tempo(),
            delay_left: self.get_active_echo_profile().time_left(),
            delay_right: self.get_active_echo_profile().time_right(),
            feedback_left: self.get_active_echo_profile().feedback_left(),
            feedback_right: self.get_active_echo_profile().feedback_right(),
            feedback_xfb_l_to_r: self.get_active_echo_profile().xfb_l_to_r(),
            feedback_xfb_r_to_l: self.get_active_echo_profile().xfb_r_to_l(),
        };

        let pitch = Pitch {
            style: profile_to_standard_pitch_style(self.get_active_pitch_profile().style()),
            amount: self
                .get_active_pitch_profile()
                .knob_position(self.is_hardtune_enabled(true)),
            character: self.get_active_pitch_profile().inst_ratio_value(),
        };

        let gender = Gender {
            style: profile_to_standard_gender_style(self.get_active_gender_profile().style()),
            amount: self.get_active_gender_profile().amount(),
        };

        let megaphone = Megaphone {
            is_enabled: self.is_megaphone_enabled(true),
            style: profile_to_standard_megaphone_style(self.get_active_megaphone_profile().style()),
            amount: self.get_active_megaphone_profile().trans_dist_amt(),
            post_gain: self.get_active_megaphone_profile().trans_postgain(),
        };

        let robot = Robot {
            is_enabled: self.is_robot_enabled(true),
            style: profile_to_standard_robot_style(self.get_active_robot_profile().style()),
            low_gain: self.get_active_robot_profile().vocoder_low_gain(),
            low_freq: self.get_active_robot_profile().vocoder_low_freq(),
            low_width: self.get_active_robot_profile().vocoder_low_bw(),
            mid_gain: self.get_active_robot_profile().vocoder_mid_gain(),
            mid_freq: self.get_active_robot_profile().vocoder_mid_freq(),
            mid_width: self.get_active_robot_profile().vocoder_mid_bw(),
            high_gain: self.get_active_robot_profile().vocoder_high_gain(),
            high_freq: self.get_active_robot_profile().vocoder_high_freq(),
            high_width: self.get_active_robot_profile().vocoder_high_bw(),
            waveform: self.get_active_robot_profile().synthosc_waveform(),
            pulse_width: self.get_active_robot_profile().synthosc_pulse_width(),
            threshold: self.get_active_robot_profile().vocoder_gate_threshold(),
            dry_mix: self.get_active_robot_profile().dry_mix(),
        };

        let hard_tune = HardTune {
            is_enabled: self.is_hardtune_enabled(true),
            style: profile_to_standard_hard_tune_style(self.get_active_hardtune_profile().style()),
            amount: self.get_active_hardtune_profile().amount(),
            rate: self.get_active_hardtune_profile().rate(),
            window: self.get_active_hardtune_profile().window(),
            source: profile_to_standard_hard_tune_source(
                &self.get_active_hardtune_profile().get_source_value(),
            ),
        };

        Some(Effects {
            active_preset,
            preset_names,
            current: ActiveEffects {
                reverb,
                echo,
                pitch,
                gender,
                megaphone,
                robot,
                hard_tune,
            },
        })
    }

    pub fn get_sampler_ipc(&self, is_device_mini: bool) -> Option<Sampler> {
        if is_device_mini {
            return None;
        }

        let mut sampler_map = HashMap::new();

        for bank in goxlr_types::SampleBank::iter() {
            let mut buttons = HashMap::new();

            for button in goxlr_types::SampleButtons::iter() {
                // Grab the sample config..
                let sample_bank = self
                    .profile
                    .settings()
                    .sample_button(standard_to_profile_sample_button(button))
                    .get_stack(standard_to_profile_sample_bank(bank));

                let mut tracks = vec![];
                for track in sample_bank.get_tracks() {
                    tracks.push(track.track().to_string())
                }

                // Create a SamplerButton
                let sampler_button = SamplerButton {
                    function: profile_to_standard_sample_playback_mode(
                        sample_bank.get_playback_mode(),
                    ),
                    order: profile_to_standard_sample_playback_order(sample_bank.get_play_order()),
                    samples: tracks,
                };
                buttons.insert(button, sampler_button);
            }

            sampler_map.insert(bank, buttons);
        }

        Some(Sampler { banks: sampler_map })
    }

    /** Regular Mute button handlers */
    fn get_mute_button(&self, fader: FaderName) -> &MuteButton {
        self.profile
            .settings()
            .mute_button(standard_to_profile_fader(fader))
    }

    fn get_mute_button_mut(&mut self, fader: FaderName) -> &mut MuteButton {
        self.profile
            .settings_mut()
            .mute_button_mut(standard_to_profile_fader(fader))
    }

    pub fn get_mute_button_behaviour(&self, fader: FaderName) -> BasicMuteFunction {
        let mute_config = self.get_mute_button(fader);
        return profile_to_standard_mute_function(*mute_config.mute_function());
    }

    pub fn set_mute_button_behaviour(&mut self, fader: FaderName, behaviour: BasicMuteFunction) {
        let mute_config = self.get_mute_button_mut(fader);
        mute_config.set_mute_function(standard_to_profile_mute_function(behaviour));
    }

    pub fn get_mute_button_state(&self, fader: FaderName) -> (bool, bool, MuteFunction) {
        let mute_config = self.get_mute_button(fader);
        let colour_map = mute_config.colour_map();

        // We should be safe to straight unwrap these, state and blink are always present.
        let muted_to_x = colour_map.state().as_ref().unwrap() == &ColourState::On;
        let muted_to_all = colour_map.blink().as_ref().unwrap() == &ColourState::On;
        let mute_function = *mute_config.mute_function();

        (muted_to_x, muted_to_all, mute_function)
    }

    pub fn get_mute_button_previous_volume(&self, fader: FaderName) -> u8 {
        self.get_mute_button(fader).previous_volume()
    }

    pub fn set_mute_button_previous_volume(&mut self, fader: FaderName, volume: u8) -> Result<()> {
        self.get_mute_button_mut(fader).set_previous_volume(volume)
    }

    pub fn set_mute_button_on(&mut self, fader: FaderName, on: bool) -> Result<()> {
        self.get_mute_button_mut(fader)
            .colour_map_mut()
            .set_state_on(on)
    }

    pub fn set_mute_button_blink(&mut self, fader: FaderName, on: bool) -> Result<()> {
        self.get_mute_button_mut(fader)
            .colour_map_mut()
            .set_blink_on(on)
    }

    /** 'Cough' / Mute Chat Button handlers.. */
    pub fn get_chat_mute_button(&self) -> &MuteChat {
        self.profile.settings().mute_chat()
    }

    pub fn get_chat_mute_button_mut(&mut self) -> &mut MuteChat {
        self.profile.settings_mut().mute_chat_mut()
    }

    pub fn get_chat_mute_button_behaviour(&self) -> BasicMuteFunction {
        let mute_config = self.get_chat_mute_button();
        return profile_to_standard_mute_function(*mute_config.cough_mute_source());
    }

    pub fn set_chat_mute_button_behaviour(&mut self, behaviour: BasicMuteFunction) {
        let mute_config = self.get_chat_mute_button_mut();
        mute_config.set_cough_mute_source(standard_to_profile_mute_function(behaviour));
    }

    pub fn set_chat_mute_button_is_held(&mut self, is_hold: bool) {
        let mute_config = self.get_chat_mute_button_mut();
        if is_hold {
            mute_config.set_cough_behaviour(CoughToggle::Hold);
        } else {
            mute_config.set_cough_behaviour(CoughToggle::Toggle);
        }
    }

    pub fn get_mute_chat_button_state(&self) -> (bool, bool, bool, MuteFunction) {
        let mute_config = self.profile.settings().mute_chat();

        // Identical behaviour, different variable locations..
        let mute_toggle = mute_config.is_cough_toggle();
        let muted_to_x = mute_config.cough_button_on();
        let muted_to_all = mute_config.blink() == &ColourState::On;
        let mute_function = *mute_config.cough_mute_source();

        (mute_toggle, muted_to_x, muted_to_all, mute_function)
    }

    pub fn set_mute_chat_button_on(&mut self, on: bool) {
        self.profile
            .settings_mut()
            .mute_chat_mut()
            .set_cough_button_on(on);
    }

    pub fn set_mute_chat_button_blink(&mut self, on: bool) {
        self.profile.settings_mut().mute_chat_mut().set_blink_on(on);
    }

    pub fn get_mute_chat_button_blink(&self) -> bool {
        self.profile.settings().mute_chat().get_blink_on()
    }

    pub fn get_mute_chat_button_on(&self) -> bool {
        self.profile.settings().mute_chat().get_cough_button_on()
    }

    pub fn get_mute_chat_button_colour_state(&self) -> ButtonStates {
        if self.get_mute_chat_button_blink() {
            return ButtonStates::Flashing;
        }

        if self.get_mute_chat_button_on() {
            return ButtonStates::Colour1;
        }

        return match self
            .profile
            .settings()
            .mute_chat()
            .colour_map()
            .get_off_style()
        {
            ColourOffStyle::Dimmed => ButtonStates::DimmedColour1,
            ColourOffStyle::Colour2 => ButtonStates::Colour2,
            ColourOffStyle::DimmedColour2 => ButtonStates::DimmedColour2,
        };
    }

    pub fn get_cough_status(&self) -> CoughButton {
        CoughButton {
            is_toggle: self.profile.settings().mute_chat().is_cough_toggle(),
            mute_type: profile_to_standard_mute_function(
                *self.profile.settings().mute_chat().cough_mute_source(),
            ),
        }
    }

    /** Fader Stuff */
    pub fn get_mic_fader_id(&self) -> u8 {
        self.profile.settings().mute_chat().mic_fader_id()
    }

    pub fn get_mic_fader(&self) -> FaderName {
        self.fader_from_id(self.profile.settings().mute_chat().mic_fader_id())
    }

    pub fn set_mic_fader(&mut self, fader: FaderName) -> Result<()> {
        self.profile
            .settings_mut()
            .mute_chat_mut()
            .set_mic_fader_id(fader as u8)
    }

    pub fn clear_mic_fader(&mut self) {
        self.profile
            .settings_mut()
            .mute_chat_mut()
            .clear_mic_fader_id();
    }

    // TODO: This can probably be cleaned with EnumIter
    pub fn fader_from_id(&self, fader: u8) -> FaderName {
        match fader {
            0 => FaderName::A,
            1 => FaderName::B,
            2 => FaderName::C,
            _ => FaderName::D,
        }
    }

    pub fn is_fader_gradient(&self, fader: FaderName) -> bool {
        self.profile
            .settings()
            .fader(standard_to_profile_fader(fader))
            .colour_map()
            .is_fader_gradient()
    }

    pub fn is_fader_meter(&self, fader: FaderName) -> bool {
        self.profile
            .settings()
            .fader(standard_to_profile_fader(fader))
            .colour_map()
            .is_fader_meter()
    }

    /** Bleep Button **/
    pub fn set_swear_button_on(&mut self, on: bool) -> Result<()> {
        // Get the colour map for the bleep button..
        self.profile
            .settings_mut()
            .simple_element_mut(SimpleElements::Swear)
            .colour_map_mut()
            .set_state_on(on)
    }

    /** Effects Bank Behaviours **/
    pub fn get_active_effect_bank(&mut self) -> EffectBankPresets {
        let current = self.profile.settings().context().selected_effects();
        profile_to_standard_preset(current)
    }

    pub fn load_effect_bank(&mut self, preset: EffectBankPresets) -> Result<()> {
        let preset = standard_to_profile_preset(preset);
        let current = self.profile.settings().context().selected_effects();

        // Ok, first thing we need to do is set the prefix in the profile..
        self.profile
            .settings_mut()
            .context_mut()
            .set_selected_effects(preset);

        // Disable the 'On' state of the existing button..
        self.profile
            .settings_mut()
            .effects_mut(current)
            .colour_map_mut()
            .set_state_on(false)?;

        // Now we need to go through all the buttons, and set their new colour state..
        let state = self
            .profile
            .settings_mut()
            .robot_effect()
            .get_preset(preset)
            .state();
        self.profile
            .settings_mut()
            .robot_effect_mut()
            .colour_map_mut()
            .set_state_on(state)?;

        let state = self
            .profile
            .settings_mut()
            .megaphone_effect()
            .get_preset(preset)
            .state();
        self.profile
            .settings_mut()
            .megaphone_effect_mut()
            .colour_map_mut()
            .set_state_on(state)?;

        let state = self
            .profile
            .settings_mut()
            .hardtune_effect()
            .get_preset(preset)
            .state();
        self.profile
            .settings_mut()
            .hardtune_effect_mut()
            .colour_map_mut()
            .set_state_on(state)?;

        // Set the new button 'On'
        self.profile
            .settings_mut()
            .effects_mut(preset)
            .colour_map_mut()
            .set_state_on(true)?;

        Ok(())
    }

    pub fn toggle_megaphone(&mut self) -> Result<()> {
        let current = self.profile.settings().context().selected_effects();

        let new_state = !self
            .profile
            .settings()
            .megaphone_effect()
            .get_preset(current)
            .state();

        self.profile
            .settings_mut()
            .megaphone_effect_mut()
            .get_preset_mut(current)
            .set_state(new_state);
        self.profile
            .settings_mut()
            .megaphone_effect_mut()
            .colour_map_mut()
            .set_state_on(new_state)
    }

    pub fn toggle_robot(&mut self) -> Result<()> {
        let current = self.profile.settings().context().selected_effects();

        let new_state = !self
            .profile
            .settings()
            .robot_effect()
            .get_preset(current)
            .state();

        self.profile
            .settings_mut()
            .robot_effect_mut()
            .get_preset_mut(current)
            .set_state(new_state);
        self.profile
            .settings_mut()
            .robot_effect_mut()
            .colour_map_mut()
            .set_state_on(new_state)
    }

    pub fn toggle_hardtune(&mut self) -> Result<()> {
        let current = self.profile.settings().context().selected_effects();

        let new_state = !self
            .profile
            .settings()
            .hardtune_effect()
            .get_preset(current)
            .state();
        self.profile
            .settings_mut()
            .hardtune_effect_mut()
            .get_preset_mut(current)
            .set_state(new_state);
        self.profile
            .settings_mut()
            .hardtune_effect_mut()
            .colour_map_mut()
            .set_state_on(new_state)
    }

    pub fn toggle_effects(&mut self) -> Result<()> {
        let state = !self
            .profile
            .settings()
            .simple_element(SimpleElements::FxClear)
            .colour_map()
            .get_state();
        self.profile
            .settings_mut()
            .simple_element_mut(SimpleElements::FxClear)
            .colour_map_mut()
            .set_state_on(state)
    }

    pub fn get_pitch_knob_position(&self) -> i8 {
        self.get_active_pitch_profile()
            .knob_position(self.is_hardtune_enabled(true))
    }

    pub fn get_pitch_encoder_position(&self) -> i8 {
        self.get_active_pitch_profile()
            .get_encoder_position(self.is_hardtune_enabled(true))
    }

    pub fn calculate_pitch_knob_position(&self, value: i8) -> i8 {
        self.get_active_pitch_profile()
            .calculate_encoder_value(value, self.is_hardtune_enabled(true))
    }

    pub fn set_pitch_knob_position(&mut self, value: i8) -> Result<()> {
        let hardtune_enabled = self.is_hardtune_enabled(true);
        let current = self.profile.settings().context().selected_effects();
        self.profile
            .settings_mut()
            .pitch_encoder_mut()
            .get_preset_mut(current)
            .set_knob_position(value, hardtune_enabled)
    }

    pub fn set_pitch_style(&mut self, style: goxlr_types::PitchStyle) -> Result<()> {
        self.get_active_pitch_profile_mut()
            .set_style(standard_to_profile_pitch_style(style));
        Ok(())
    }

    pub fn get_pitch_mode(&self) -> u8 {
        self.get_active_pitch_profile()
            .pitch_mode(self.is_hardtune_enabled(true))
    }

    pub fn get_pitch_resolution(&self) -> u8 {
        self.get_active_pitch_profile()
            .pitch_resolution(self.is_hardtune_enabled(true))
    }

    pub fn get_active_pitch_profile(&self) -> &PitchEncoder {
        let current = self.profile.settings().context().selected_effects();
        self.profile.settings().pitch_encoder().get_preset(current)
    }

    pub fn get_active_pitch_profile_mut(&mut self) -> &mut PitchEncoder {
        let current = self.profile.settings().context().selected_effects();
        self.profile
            .settings_mut()
            .pitch_encoder_mut()
            .get_preset_mut(current)
    }

    pub fn get_gender_value(&self) -> i8 {
        let current = self.profile.settings().context().selected_effects();
        self.profile
            .settings()
            .gender_encoder()
            .get_preset(current)
            .knob_position()
    }

    pub fn set_gender_value(&mut self, value: i8) -> Result<()> {
        let current = self.profile.settings().context().selected_effects();
        self.profile
            .settings_mut()
            .gender_encoder_mut()
            .get_preset_mut(current)
            .set_knob_position(value)
    }

    pub fn set_gender_style(&mut self, style: goxlr_types::GenderStyle) -> Result<()> {
        self.get_active_gender_profile_mut()
            .set_style(standard_to_profile_gender_style(style));
        Ok(())
    }

    pub fn get_active_gender_profile(&self) -> &GenderEncoder {
        let current = self.profile.settings().context().selected_effects();
        self.profile.settings().gender_encoder().get_preset(current)
    }

    pub fn get_active_gender_profile_mut(&mut self) -> &mut GenderEncoder {
        let current = self.profile.settings().context().selected_effects();
        self.profile
            .settings_mut()
            .gender_encoder_mut()
            .get_preset_mut(current)
    }

    pub fn get_reverb_value(&self) -> i8 {
        let current = self.profile.settings().context().selected_effects();
        self.profile
            .settings()
            .reverb_encoder()
            .get_preset(current)
            .knob_position()
    }

    pub fn set_reverb_value(&mut self, value: i8) -> Result<()> {
        let current = self.profile.settings().context().selected_effects();
        self.profile
            .settings_mut()
            .reverb_encoder_mut()
            .get_preset_mut(current)
            .set_knob_position(value)
    }

    pub fn set_reverb_style(&mut self, style: goxlr_types::ReverbStyle) -> Result<()> {
        self.get_active_reverb_profile_mut()
            .set_style(standard_to_profile_reverb_style(style))?;
        Ok(())
    }

    pub fn get_active_reverb_profile(&self) -> &ReverbEncoder {
        let current = self.profile.settings().context().selected_effects();
        self.profile.settings().reverb_encoder().get_preset(current)
    }

    pub fn get_active_reverb_profile_mut(&mut self) -> &mut ReverbEncoder {
        let current = self.profile.settings().context().selected_effects();
        self.profile
            .settings_mut()
            .reverb_encoder_mut()
            .get_preset_mut(current)
    }

    pub fn get_echo_value(&self) -> i8 {
        let current = self.profile.settings().context().selected_effects();
        self.profile
            .settings()
            .echo_encoder()
            .get_preset(current)
            .knob_position()
    }

    pub fn set_echo_value(&mut self, value: i8) -> Result<()> {
        let current = self.profile.settings().context().selected_effects();
        self.profile
            .settings_mut()
            .echo_encoder_mut()
            .get_preset_mut(current)
            .set_knob_position(value)
    }

    pub fn set_echo_style(&mut self, style: goxlr_types::EchoStyle) -> Result<()> {
        self.get_active_echo_profile_mut()
            .set_style(standard_to_profile_echo_style(style))?;
        Ok(())
    }

    pub fn get_active_echo_profile(&self) -> &EchoEncoder {
        let current = self.profile.settings().context().selected_effects();
        self.profile.settings().echo_encoder().get_preset(current)
    }

    pub fn get_active_echo_profile_mut(&mut self) -> &mut EchoEncoder {
        let current = self.profile.settings().context().selected_effects();
        self.profile
            .settings_mut()
            .echo_encoder_mut()
            .get_preset_mut(current)
    }

    pub fn set_megaphone_style(&mut self, style: goxlr_types::MegaphoneStyle) -> Result<()> {
        self.get_active_megaphone_profile_mut()
            .set_style(standard_to_profile_megaphone_style(style))?;
        Ok(())
    }

    pub fn get_active_megaphone_profile(&self) -> &MegaphoneEffect {
        let current = self.profile.settings().context().selected_effects();
        self.profile
            .settings()
            .megaphone_effect()
            .get_preset(current)
    }

    pub fn get_active_megaphone_profile_mut(&mut self) -> &mut MegaphoneEffect {
        let current = self.profile.settings().context().selected_effects();
        self.profile
            .settings_mut()
            .megaphone_effect_mut()
            .get_preset_mut(current)
    }

    pub fn set_robot_style(&mut self, style: goxlr_types::RobotStyle) -> Result<()> {
        self.get_active_robot_profile_mut()
            .set_style(standard_to_profile_robot_style(style))?;
        Ok(())
    }

    pub fn get_active_robot_profile(&self) -> &RobotEffect {
        let current = self.profile.settings().context().selected_effects();
        self.profile.settings().robot_effect().get_preset(current)
    }

    pub fn get_active_robot_profile_mut(&mut self) -> &mut RobotEffect {
        let current = self.profile.settings().context().selected_effects();
        self.profile
            .settings_mut()
            .robot_effect_mut()
            .get_preset_mut(current)
    }

    pub fn set_hardtune_style(&mut self, style: goxlr_types::HardTuneStyle) -> Result<()> {
        self.get_active_hardtune_profile_mut()
            .set_style(standard_to_profile_hard_tune_style(style))?;
        Ok(())
    }

    pub fn set_hardtune_source(&mut self, source: goxlr_types::HardTuneSource) -> Result<()> {
        self.get_active_hardtune_profile_mut()
            .set_source(standard_to_profile_hard_tune_source(source));
        Ok(())
    }

    pub fn get_hardtune_source(&self) -> goxlr_types::HardTuneSource {
        profile_to_standard_hard_tune_source(&self.get_active_hardtune_profile().get_source_value())
    }

    pub fn get_active_hardtune_profile(&self) -> &HardTuneEffect {
        let current = self.profile.settings().context().selected_effects();
        self.profile
            .settings()
            .hardtune_effect()
            .get_preset(current)
    }

    pub fn get_active_hardtune_profile_mut(&mut self) -> &mut HardTuneEffect {
        let current = self.profile.settings().context().selected_effects();
        self.profile
            .settings_mut()
            .hardtune_effect_mut()
            .get_preset_mut(current)
    }

    pub fn is_active_hardtune_source_all(&self) -> bool {
        if let Some(source) = self.get_active_hardtune_profile().source() {
            return source == &HardTuneSource::All;
        }

        // If it's not set, assume default behaviour of 'All'
        true
    }

    pub fn get_active_hardtune_source(&self) -> InputDevice {
        let source = self.get_active_hardtune_profile().source();
        match source.unwrap() {
            HardTuneSource::Music => InputDevice::Music,
            HardTuneSource::Game => InputDevice::Game,
            HardTuneSource::LineIn => InputDevice::LineIn,
            HardTuneSource::System => InputDevice::System,

            // This should never really be called when Source is All, return a default.
            HardTuneSource::All => InputDevice::Music,
        }
    }

    pub fn is_fx_enabled(&self) -> bool {
        self.profile
            .settings()
            .simple_element(SimpleElements::FxClear)
            .colour_map()
            .get_state()
    }

    pub fn is_megaphone_enabled(&self, ignore_fx_state: bool) -> bool {
        if !ignore_fx_state && !self.is_fx_enabled() {
            return false;
        }
        self.profile
            .settings()
            .megaphone_effect()
            .colour_map()
            .get_state()
    }

    pub fn is_robot_enabled(&self, ignore_fx_state: bool) -> bool {
        if !ignore_fx_state && !self.is_fx_enabled() {
            return false;
        }
        self.profile
            .settings()
            .robot_effect()
            .colour_map()
            .get_state()
    }

    pub fn is_hardtune_enabled(&self, ignore_fx_state: bool) -> bool {
        if !ignore_fx_state && !self.is_fx_enabled() {
            return false;
        }
        self.profile
            .settings()
            .hardtune_effect()
            .colour_map()
            .get_state()
    }

    /** Sampler Related **/
    pub fn load_sample_bank(&mut self, bank: goxlr_types::SampleBank) -> Result<()> {
        let bank = standard_to_profile_sample_bank(bank);
        let current = self.profile.settings().context().selected_sample();

        // Set the new context..
        self.profile
            .settings_mut()
            .context_mut()
            .set_selected_sample(bank);

        // Turn off existing bank..
        get_profile_colour_map_mut(
            self.profile.settings_mut(),
            map_sample_bank_to_colour_target(current),
        )
        .set_state_on(false)?;

        // Turn on New Bank..
        get_profile_colour_map_mut(
            self.profile.settings_mut(),
            map_sample_bank_to_colour_target(bank),
        )
        .set_state_on(true)?;

        self.sync_sample_colours(bank)?;
        Ok(())
    }

    pub fn get_active_sample_bank(&self) -> goxlr_types::SampleBank {
        profile_to_standard_sample_bank(self.profile.settings().context().selected_sample())
    }

    pub fn get_sample_playback_mode(
        &self,
        button: goxlr_types::SampleButtons,
    ) -> SamplePlaybackMode {
        let bank = self.profile.settings().context().selected_sample();
        let stack = self
            .profile
            .settings()
            .sample_button(standard_to_profile_sample_button(button))
            .get_stack(bank);

        profile_to_standard_sample_playback_mode(stack.get_playback_mode())
    }

    pub fn sync_sample_if_active(&mut self, target: SamplerColourTargets) -> Result<()> {
        let current = self.profile.settings().context().selected_sample();
        let bank = standard_sample_colour_to_profile_bank(target);

        if bank == current {
            self.sync_sample_colours(bank)?;
        }
        Ok(())
    }

    pub fn sync_sample_colours(&mut self, bank: SampleBank) -> Result<()> {
        // When loading a bank, the colour settings from the SampleBank button get migrated
        // across to the sample buttons, which are then used to display (it's a little convoluted!)
        let colour_map = get_profile_colour_map(
            self.profile.settings_mut(),
            map_sample_bank_to_colour_target(bank),
        );

        let on_colour = Colour::from(colour_map.colour_or_default(0));
        let off_colour = Colour::from(colour_map.colour_or_default(2));

        for sample in get_sampler_colour_targets() {
            let map = get_profile_colour_map_mut(
                self.profile.settings_mut(),
                standard_to_colour_target(sample),
            );

            map.set_colour(0, Colour::from(&on_colour))?;
            map.set_colour(1, Colour::from(&off_colour))?;
        }

        // TODO: When loading a bank, we should check for the existence of samples
        // If they're missing, remove them from the stack.

        Ok(())
    }

    pub fn current_sample_bank_has_samples(&self, button: goxlr_types::SampleButtons) -> bool {
        let bank = self.profile.settings().context().selected_sample();
        let stack = self
            .profile
            .settings()
            .sample_button(standard_to_profile_sample_button(button))
            .get_stack(bank);

        if stack.get_track_count() == 0 {
            return false;
        }
        true
    }

    pub fn get_next_track(&mut self, button: goxlr_types::SampleButtons) -> Result<AudioFile> {
        let bank = self.profile.settings().context().selected_sample();
        let track = self
            .profile
            .settings_mut()
            .sample_button_mut(standard_to_profile_sample_button(button))
            .get_stack_mut(bank)
            .get_next_track();

        if let Some(track) = track {
            let mut gain = None;
            let mut start_pct = None;
            let mut stop_pct = None;

            if track.normalized_gain() != 1.0 {
                gain = Some(track.normalized_gain());
            }

            if track.start_position() != 0.0 {
                start_pct = Some(track.start_position() as f64);
            }

            if track.end_position() != 100.0 {
                stop_pct = Some(track.end_position() as f64);
            }

            return Ok(AudioFile {
                file: PathBuf::from(track.track()),
                gain,
                start_pct,
                stop_pct,
                fade_on_stop: false,
            });
        }

        Err(anyhow!("Unable to Find Track to play!"))
    }

    pub fn is_sample_active(&self, button: goxlr_types::SampleButtons) -> bool {
        self.profile
            .settings()
            .sample_button(standard_to_profile_sample_button(button))
            .colour_map()
            .get_state()
    }

    pub fn set_sample_button_state(
        &mut self,
        button: goxlr_types::SampleButtons,
        state: bool,
    ) -> Result<()> {
        self.profile
            .settings_mut()
            .sample_button_mut(standard_to_profile_sample_button(button))
            .colour_map_mut()
            .set_state_on(state)
    }

    pub fn set_sample_button_blink(
        &mut self,
        button: goxlr_types::SampleButtons,
        state: bool,
    ) -> Result<()> {
        self.profile
            .settings_mut()
            .sample_button_mut(standard_to_profile_sample_button(button))
            .colour_map_mut()
            .set_blink_on(state)
    }

    pub fn is_sample_clear_active(&self) -> bool {
        self.profile
            .settings()
            .sample_button(SampleButtons::Clear)
            .colour_map()
            .is_blink()
    }

    pub fn set_sample_clear_active(&mut self, active: bool) -> Result<()> {
        self.profile
            .settings_mut()
            .sample_button_mut(SampleButtons::Clear)
            .colour_map_mut()
            .set_blink_on(active)
    }

    pub fn clear_all_samples(&mut self, button: goxlr_types::SampleButtons) {
        let bank = self.profile.settings().context().selected_sample();

        self.profile
            .settings_mut()
            .sample_button_mut(standard_to_profile_sample_button(button))
            .get_stack_mut(bank)
            .clear_tracks();
    }

    /** Colour Changing Code **/
    pub fn set_button_colours(
        &mut self,
        target: ButtonColourTargets,
        colour_one: String,
        colour_two: Option<&String>,
    ) -> Result<()> {
        let colour_target = standard_to_colour_target(target);
        let colours = get_profile_colour_map_mut(self.profile.settings_mut(), colour_target);

        colours.set_colour(0, Colour::fromrgb(colour_one.as_str())?)?;
        if let Some(two) = colour_two {
            colours.set_colour(1, Colour::fromrgb(two.as_str())?)?;
        }
        Ok(())
    }

    pub fn set_simple_colours(
        &mut self,
        target: SimpleColourTargets,
        colour_one: String,
    ) -> Result<()> {
        if target == SimpleColourTargets::Global {
            // The 'Global' Colour as defined in the GoXLR App is a 'special' case, where it will
            // set every target to the same colour. (along with a couple of other tweaks).
            warn!("Global Colour Setting not Implemented");

            // Set the config value anyway..
            let colour_map = self
                .profile
                .settings_mut()
                .simple_element_mut(SimpleElements::GlobalColour)
                .colour_map_mut();
            colour_map.set_colour(0, Colour::fromrgb(colour_one.as_str())?)?;

            return Ok(());
        }

        let colour_target = standard_to_profile_simple_colour(target);
        let colours = get_profile_colour_map_mut(self.profile.settings_mut(), colour_target);

        colours.set_colour(0, Colour::fromrgb(colour_one.as_str())?)?;
        Ok(())
    }

    pub fn set_encoder_colours(
        &mut self,
        target: EncoderColourTargets,
        colour_one: String,
        colour_two: String,
        colour_three: String,
    ) -> Result<()> {
        let colour_target = standard_to_profile_encoder_colour(target);
        let colours = get_profile_colour_map_mut(self.profile.settings_mut(), colour_target);

        colours.set_colour(0, Colour::fromrgb(colour_one.as_str())?)?;
        colours.set_colour(1, Colour::fromrgb(colour_two.as_str())?)?;
        colours.set_colour(2, Colour::fromrgb(colour_three.as_str())?)?;

        Ok(())
    }

    pub fn set_sampler_colours(
        &mut self,
        target: SamplerColourTargets,
        colour_one: String,
        colour_two: String,
        colour_three: String,
    ) -> Result<()> {
        let colour_target = standard_to_sample_colour(target);
        let colours = get_profile_colour_map_mut(self.profile.settings_mut(), colour_target);

        colours.set_colour(0, Colour::fromrgb(colour_one.as_str())?)?;
        colours.set_colour(1, Colour::fromrgb(colour_two.as_str())?)?;
        colours.set_colour(2, Colour::fromrgb(colour_three.as_str())?)?;

        Ok(())
    }

    pub fn set_sampler_off_style(
        &mut self,
        target: SamplerColourTargets,
        off_style: BasicColourOffStyle,
    ) -> Result<()> {
        let colour_target = standard_to_sample_colour(target);
        get_profile_colour_map_mut(self.profile.settings_mut(), colour_target)
            .set_off_style(standard_to_profile_colour_off_style(off_style))
    }

    pub fn set_sampler_function(
        &mut self,
        bank: goxlr_types::SampleBank,
        button: goxlr_types::SampleButtons,
        mode: SamplePlaybackMode,
    ) {
        self.profile
            .settings_mut()
            .sample_button_mut(standard_to_profile_sample_button(button))
            .get_stack_mut(standard_to_profile_sample_bank(bank))
            .set_playback_mode(Some(standard_to_profile_sample_playback_mode(mode)));
    }

    pub fn set_sampler_play_order(
        &mut self,
        bank: goxlr_types::SampleBank,
        button: goxlr_types::SampleButtons,
        order: SamplePlayOrder,
    ) {
        self.profile
            .settings_mut()
            .sample_button_mut(standard_to_profile_sample_button(button))
            .get_stack_mut(standard_to_profile_sample_bank(bank))
            .set_play_order(Some(standard_to_profile_sample_playback_order(order)));
    }

    pub fn add_sample_file(
        &mut self,
        bank: goxlr_types::SampleBank,
        button: goxlr_types::SampleButtons,
        file: String,
    ) -> &mut Track {
        // Create a new 'Track' (Oddly, positions are a percentage :D)..
        let track = Track {
            track: file,
            start_position: 0.0,
            end_position: 100.0,
            normalized_gain: 1.0,
        };

        // Add this to the list, then return the track..
        self.profile
            .settings_mut()
            .sample_button_mut(standard_to_profile_sample_button(button))
            .get_stack_mut(standard_to_profile_sample_bank(bank))
            .add_track(track)
    }

    pub fn remove_sample_file_by_index(
        &mut self,
        bank: goxlr_types::SampleBank,
        button: goxlr_types::SampleButtons,
        index: usize,
    ) {
        self.profile
            .settings_mut()
            .sample_button_mut(standard_to_profile_sample_button(button))
            .get_stack_mut(standard_to_profile_sample_bank(bank))
            .remove_track_by_index(index);
    }

    pub fn set_button_off_style(
        &mut self,
        target: ButtonColourTargets,
        off_style: BasicColourOffStyle,
    ) -> Result<()> {
        let colour_target = standard_to_colour_target(target);
        get_profile_colour_map_mut(self.profile.settings_mut(), colour_target)
            .set_off_style(standard_to_profile_colour_off_style(off_style))
    }

    // TODO: We can probably do better with grouping these so they can be reused.
    pub fn set_group_button_colours(
        &mut self,
        group: ButtonColourGroups,
        colour_one: String,
        colour_two: Option<String>,
    ) -> Result<()> {
        match group {
            ButtonColourGroups::FaderMute => {
                self.set_button_colours(
                    ButtonColourTargets::Fader1Mute,
                    colour_one.clone(),
                    colour_two.as_ref(),
                )?;
                self.set_button_colours(
                    ButtonColourTargets::Fader2Mute,
                    colour_one.clone(),
                    colour_two.as_ref(),
                )?;
                self.set_button_colours(
                    ButtonColourTargets::Fader3Mute,
                    colour_one.clone(),
                    colour_two.as_ref(),
                )?;
                self.set_button_colours(
                    ButtonColourTargets::Fader4Mute,
                    colour_one,
                    colour_two.as_ref(),
                )?;
            }
            ButtonColourGroups::EffectSelector => {
                self.set_button_colours(
                    ButtonColourTargets::EffectSelect1,
                    colour_one.clone(),
                    colour_two.as_ref(),
                )?;
                self.set_button_colours(
                    ButtonColourTargets::EffectSelect2,
                    colour_one.clone(),
                    colour_two.as_ref(),
                )?;
                self.set_button_colours(
                    ButtonColourTargets::EffectSelect3,
                    colour_one.clone(),
                    colour_two.as_ref(),
                )?;
                self.set_button_colours(
                    ButtonColourTargets::EffectSelect4,
                    colour_one.clone(),
                    colour_two.as_ref(),
                )?;
                self.set_button_colours(
                    ButtonColourTargets::EffectSelect5,
                    colour_one.clone(),
                    colour_two.as_ref(),
                )?;
                self.set_button_colours(
                    ButtonColourTargets::EffectSelect6,
                    colour_one,
                    colour_two.as_ref(),
                )?;
            }
            ButtonColourGroups::SampleBankSelector => {
                self.set_button_colours(
                    ButtonColourTargets::SamplerSelectA,
                    colour_one.clone(),
                    colour_two.as_ref(),
                )?;
                self.set_button_colours(
                    ButtonColourTargets::SamplerSelectB,
                    colour_one.clone(),
                    colour_two.as_ref(),
                )?;
                self.set_button_colours(
                    ButtonColourTargets::SamplerSelectC,
                    colour_one,
                    colour_two.as_ref(),
                )?;
            }
            ButtonColourGroups::SamplerButtons => {
                self.set_button_colours(
                    ButtonColourTargets::SamplerTopLeft,
                    colour_one.clone(),
                    colour_two.as_ref(),
                )?;
                self.set_button_colours(
                    ButtonColourTargets::SamplerTopRight,
                    colour_one.clone(),
                    colour_two.as_ref(),
                )?;
                self.set_button_colours(
                    ButtonColourTargets::SamplerBottomLeft,
                    colour_one.clone(),
                    colour_two.as_ref(),
                )?;
                self.set_button_colours(
                    ButtonColourTargets::SamplerBottomRight,
                    colour_one.clone(),
                    colour_two.as_ref(),
                )?;
                self.set_button_colours(
                    ButtonColourTargets::SamplerClear,
                    colour_one,
                    colour_two.as_ref(),
                )?;
            }
        }

        Ok(())
    }

    pub fn set_group_button_off_style(
        &mut self,
        target: ButtonColourGroups,
        off_style: BasicColourOffStyle,
    ) -> Result<()> {
        match target {
            ButtonColourGroups::FaderMute => {
                self.set_button_off_style(ButtonColourTargets::Fader1Mute, off_style)?;
                self.set_button_off_style(ButtonColourTargets::Fader2Mute, off_style)?;
                self.set_button_off_style(ButtonColourTargets::Fader3Mute, off_style)?;
                self.set_button_off_style(ButtonColourTargets::Fader4Mute, off_style)?;
            }
            ButtonColourGroups::EffectSelector => {
                self.set_button_off_style(ButtonColourTargets::EffectSelect1, off_style)?;
                self.set_button_off_style(ButtonColourTargets::EffectSelect2, off_style)?;
                self.set_button_off_style(ButtonColourTargets::EffectSelect3, off_style)?;
                self.set_button_off_style(ButtonColourTargets::EffectSelect4, off_style)?;
                self.set_button_off_style(ButtonColourTargets::EffectSelect5, off_style)?;
                self.set_button_off_style(ButtonColourTargets::EffectSelect6, off_style)?;
            }
            ButtonColourGroups::SampleBankSelector => {
                self.set_button_off_style(ButtonColourTargets::SamplerSelectA, off_style)?;
                self.set_button_off_style(ButtonColourTargets::SamplerSelectB, off_style)?;
                self.set_button_off_style(ButtonColourTargets::SamplerSelectC, off_style)?;
            }
            ButtonColourGroups::SamplerButtons => {
                self.set_button_off_style(ButtonColourTargets::SamplerTopLeft, off_style)?;
                self.set_button_off_style(ButtonColourTargets::SamplerTopRight, off_style)?;
                self.set_button_off_style(ButtonColourTargets::SamplerBottomLeft, off_style)?;
                self.set_button_off_style(ButtonColourTargets::SamplerBottomRight, off_style)?;
                self.set_button_off_style(ButtonColourTargets::SamplerClear, off_style)?;
            }
        }
        Ok(())
    }

    /** Generic Stuff **/
    pub fn get_button_colour_state(&self, button: Buttons) -> ButtonStates {
        let colour_map = self.get_button_colour_map(button);

        if let Some(blink) = colour_map.blink() {
            if blink == &ColourState::On {
                return ButtonStates::Flashing;
            }
        }

        if let Some(state) = colour_map.state() {
            if state == &ColourState::On {
                return ButtonStates::Colour1;
            }
        }

        // Button is turned off, so go return the 'Off Style'
        return match colour_map.get_off_style() {
            ColourOffStyle::Dimmed => ButtonStates::DimmedColour1,
            ColourOffStyle::Colour2 => ButtonStates::Colour2,
            ColourOffStyle::DimmedColour2 => ButtonStates::DimmedColour2,
        };
    }

    pub fn profile(&self) -> &Profile {
        &self.profile
    }

    pub fn profile_mut(&mut self) -> &mut Profile {
        &mut self.profile
    }
}

fn profile_to_standard_input(value: InputChannels) -> InputDevice {
    match value {
        InputChannels::Mic => InputDevice::Microphone,
        InputChannels::Chat => InputDevice::Chat,
        InputChannels::Music => InputDevice::Music,
        InputChannels::Game => InputDevice::Game,
        InputChannels::Console => InputDevice::Console,
        InputChannels::LineIn => InputDevice::LineIn,
        InputChannels::System => InputDevice::System,
        InputChannels::Sample => InputDevice::Samples,
    }
}

fn standard_input_to_profile(value: InputDevice) -> InputChannels {
    match value {
        InputDevice::Microphone => InputChannels::Mic,
        InputDevice::Chat => InputChannels::Chat,
        InputDevice::Music => InputChannels::Music,
        InputDevice::Game => InputChannels::Game,
        InputDevice::Console => InputChannels::Console,
        InputDevice::LineIn => InputChannels::LineIn,
        InputDevice::System => InputChannels::System,
        InputDevice::Samples => InputChannels::Sample,
    }
}

fn profile_to_standard_output(value: OutputChannels) -> OutputDevice {
    match value {
        OutputChannels::Headphones => OutputDevice::Headphones,
        OutputChannels::Broadcast => OutputDevice::BroadcastMix,
        OutputChannels::LineOut => OutputDevice::LineOut,
        OutputChannels::ChatMic => OutputDevice::ChatMic,
        OutputChannels::Sampler => OutputDevice::Sampler,
    }
}

fn standard_output_to_profile(value: OutputDevice) -> OutputChannels {
    match value {
        OutputDevice::Headphones => OutputChannels::Headphones,
        OutputDevice::BroadcastMix => OutputChannels::Broadcast,
        OutputDevice::LineOut => OutputChannels::LineOut,
        OutputDevice::ChatMic => OutputChannels::ChatMic,
        OutputDevice::Sampler => OutputChannels::Sampler,
    }
}

fn profile_to_standard_mute_function(value: MuteFunction) -> BasicMuteFunction {
    match value {
        MuteFunction::All => BasicMuteFunction::All,
        MuteFunction::ToStream => BasicMuteFunction::ToStream,
        MuteFunction::ToVoiceChat => BasicMuteFunction::ToVoiceChat,
        MuteFunction::ToPhones => BasicMuteFunction::ToPhones,
        MuteFunction::ToLineOut => BasicMuteFunction::ToLineOut,
    }
}

fn standard_to_profile_mute_function(value: BasicMuteFunction) -> MuteFunction {
    match value {
        BasicMuteFunction::All => MuteFunction::All,
        BasicMuteFunction::ToStream => MuteFunction::ToStream,
        BasicMuteFunction::ToVoiceChat => MuteFunction::ToVoiceChat,
        BasicMuteFunction::ToPhones => MuteFunction::ToPhones,
        BasicMuteFunction::ToLineOut => MuteFunction::ToLineOut,
    }
}

fn standard_to_profile_fader_display(value: BasicColourDisplay) -> ColourDisplay {
    match value {
        BasicColourDisplay::TwoColour => ColourDisplay::TwoColour,
        BasicColourDisplay::Gradient => ColourDisplay::Gradient,
        BasicColourDisplay::Meter => ColourDisplay::Meter,
        BasicColourDisplay::GradientMeter => ColourDisplay::GradientMeter,
    }
}

#[allow(dead_code)]
fn profile_to_standard_fader_display(value: ColourDisplay) -> BasicColourDisplay {
    match value {
        ColourDisplay::TwoColour => BasicColourDisplay::TwoColour,
        ColourDisplay::Gradient => BasicColourDisplay::Gradient,
        ColourDisplay::Meter => BasicColourDisplay::Meter,
        ColourDisplay::GradientMeter => BasicColourDisplay::GradientMeter,
    }
}

fn standard_to_profile_colour_off_style(value: BasicColourOffStyle) -> ColourOffStyle {
    match value {
        BasicColourOffStyle::Dimmed => ColourOffStyle::Dimmed,
        BasicColourOffStyle::Colour2 => ColourOffStyle::Colour2,
        BasicColourOffStyle::DimmedColour2 => ColourOffStyle::DimmedColour2,
    }
}

fn profile_to_standard_colour_off_style(value: ColourOffStyle) -> BasicColourOffStyle {
    match value {
        ColourOffStyle::Dimmed => BasicColourOffStyle::Dimmed,
        ColourOffStyle::Colour2 => BasicColourOffStyle::Colour2,
        ColourOffStyle::DimmedColour2 => BasicColourOffStyle::DimmedColour2,
    }
}

fn profile_to_standard_channel(value: FullChannelList) -> ChannelName {
    match value {
        FullChannelList::Mic => ChannelName::Mic,
        FullChannelList::Chat => ChannelName::Chat,
        FullChannelList::Music => ChannelName::Music,
        FullChannelList::Game => ChannelName::Game,
        FullChannelList::Console => ChannelName::Console,
        FullChannelList::LineIn => ChannelName::LineIn,
        FullChannelList::System => ChannelName::System,
        FullChannelList::Sample => ChannelName::Sample,
        FullChannelList::Headphones => ChannelName::Headphones,
        FullChannelList::MicMonitor => ChannelName::MicMonitor,
        FullChannelList::LineOut => ChannelName::LineOut,
    }
}

fn standard_to_profile_channel(value: ChannelName) -> FullChannelList {
    match value {
        ChannelName::Mic => FullChannelList::Mic,
        ChannelName::Chat => FullChannelList::Chat,
        ChannelName::Music => FullChannelList::Music,
        ChannelName::Game => FullChannelList::Game,
        ChannelName::Console => FullChannelList::Console,
        ChannelName::LineIn => FullChannelList::LineIn,
        ChannelName::System => FullChannelList::System,
        ChannelName::Sample => FullChannelList::Sample,
        ChannelName::Headphones => FullChannelList::Headphones,
        ChannelName::MicMonitor => FullChannelList::MicMonitor,
        ChannelName::LineOut => FullChannelList::LineOut,
    }
}

#[allow(dead_code)]
fn profile_to_standard_sample_bank(bank: SampleBank) -> goxlr_types::SampleBank {
    match bank {
        SampleBank::A => goxlr_types::SampleBank::A,
        SampleBank::B => goxlr_types::SampleBank::B,
        SampleBank::C => goxlr_types::SampleBank::C,
    }
}

fn standard_to_profile_sample_bank(bank: goxlr_types::SampleBank) -> SampleBank {
    match bank {
        goxlr_types::SampleBank::A => SampleBank::A,
        goxlr_types::SampleBank::B => SampleBank::B,
        goxlr_types::SampleBank::C => SampleBank::C,
    }
}

fn standard_to_profile_sample_button(button: goxlr_types::SampleButtons) -> SampleButtons {
    match button {
        goxlr_types::SampleButtons::TopLeft => TopLeft,
        goxlr_types::SampleButtons::TopRight => TopRight,
        goxlr_types::SampleButtons::BottomLeft => BottomLeft,
        goxlr_types::SampleButtons::BottomRight => BottomRight,
    }
}

fn profile_to_standard_sample_button(button: SampleButtons) -> goxlr_types::SampleButtons {
    match button {
        TopLeft => goxlr_types::SampleButtons::TopLeft,
        TopRight => goxlr_types::SampleButtons::TopRight,
        BottomLeft => goxlr_types::SampleButtons::BottomLeft,
        BottomRight => goxlr_types::SampleButtons::BottomRight,
        _ => goxlr_types::SampleButtons::TopLeft,
    }
}

fn standard_to_profile_sample_playback_mode(mode: SamplePlaybackMode) -> PlaybackMode {
    match mode {
        SamplePlaybackMode::PlayNext => PlaybackMode::PlayNext,
        SamplePlaybackMode::PlayStop => PlaybackMode::PlayStop,
        SamplePlaybackMode::PlayFade => PlaybackMode::PlayFade,
        SamplePlaybackMode::StopOnRelease => PlaybackMode::StopOnRelease,
        SamplePlaybackMode::FadeOnRelease => PlaybackMode::FadeOnRelease,
        SamplePlaybackMode::Loop => PlaybackMode::Loop,
    }
}

fn profile_to_standard_sample_playback_mode(mode: PlaybackMode) -> SamplePlaybackMode {
    match mode {
        PlaybackMode::PlayNext => SamplePlaybackMode::PlayNext,
        PlaybackMode::PlayStop => SamplePlaybackMode::PlayStop,
        PlaybackMode::PlayFade => SamplePlaybackMode::PlayFade,
        PlaybackMode::StopOnRelease => SamplePlaybackMode::StopOnRelease,
        PlaybackMode::FadeOnRelease => SamplePlaybackMode::FadeOnRelease,
        PlaybackMode::Loop => SamplePlaybackMode::Loop,
    }
}

fn profile_to_standard_sample_playback_order(order: PlayOrder) -> SamplePlayOrder {
    match order {
        PlayOrder::Sequential => SamplePlayOrder::Sequential,
        PlayOrder::Random => SamplePlayOrder::Random,
    }
}

fn standard_to_profile_sample_playback_order(order: SamplePlayOrder) -> PlayOrder {
    match order {
        SamplePlayOrder::Sequential => PlayOrder::Sequential,
        SamplePlayOrder::Random => PlayOrder::Random,
    }
}

#[allow(dead_code)]
fn sample_bank_to_simple_element(bank: SampleBank) -> SimpleElements {
    match bank {
        SampleBank::A => SimpleElements::SampleBankA,
        SampleBank::B => SimpleElements::SampleBankB,
        SampleBank::C => SimpleElements::SampleBankC,
    }
}

#[allow(dead_code)]
fn profile_to_standard_preset(value: Preset) -> EffectBankPresets {
    match value {
        Preset::Preset1 => EffectBankPresets::Preset1,
        Preset::Preset2 => EffectBankPresets::Preset2,
        Preset::Preset3 => EffectBankPresets::Preset3,
        Preset::Preset4 => EffectBankPresets::Preset4,
        Preset::Preset5 => EffectBankPresets::Preset5,
        Preset::Preset6 => EffectBankPresets::Preset6,
    }
}

fn standard_to_profile_preset(value: EffectBankPresets) -> Preset {
    match value {
        EffectBankPresets::Preset1 => Preset::Preset1,
        EffectBankPresets::Preset2 => Preset::Preset2,
        EffectBankPresets::Preset3 => Preset::Preset3,
        EffectBankPresets::Preset4 => Preset::Preset4,
        EffectBankPresets::Preset5 => Preset::Preset5,
        EffectBankPresets::Preset6 => Preset::Preset6,
    }
}

fn standard_to_profile_fader(value: FaderName) -> Faders {
    match value {
        FaderName::A => Faders::A,
        FaderName::B => Faders::B,
        FaderName::C => Faders::C,
        FaderName::D => Faders::D,
    }
}

fn get_colour_map_from_button(profile: &ProfileSettings, button: Buttons) -> &ColourMap {
    get_profile_colour_map(profile, map_button_to_colour_target(button))
}

fn map_button_to_colour_target(button: Buttons) -> ColourTargets {
    match button {
        Buttons::Fader1Mute => ColourTargets::Fader1Mute,
        Buttons::Fader2Mute => ColourTargets::Fader2Mute,
        Buttons::Fader3Mute => ColourTargets::Fader3Mute,
        Buttons::Fader4Mute => ColourTargets::Fader4Mute,
        Buttons::Bleep => ColourTargets::Bleep,
        Buttons::MicrophoneMute => ColourTargets::MicrophoneMute,
        Buttons::EffectSelect1 => ColourTargets::EffectSelect1,
        Buttons::EffectSelect2 => ColourTargets::EffectSelect2,
        Buttons::EffectSelect3 => ColourTargets::EffectSelect3,
        Buttons::EffectSelect4 => ColourTargets::EffectSelect4,
        Buttons::EffectSelect5 => ColourTargets::EffectSelect5,
        Buttons::EffectSelect6 => ColourTargets::EffectSelect6,
        Buttons::EffectFx => ColourTargets::EffectFx,
        Buttons::EffectMegaphone => ColourTargets::EffectMegaphone,
        Buttons::EffectRobot => ColourTargets::EffectRobot,
        Buttons::EffectHardTune => ColourTargets::EffectHardTune,
        Buttons::SamplerSelectA => ColourTargets::SamplerSelectA,
        Buttons::SamplerSelectB => ColourTargets::SamplerSelectB,
        Buttons::SamplerSelectC => ColourTargets::SamplerSelectC,
        Buttons::SamplerTopLeft => ColourTargets::SamplerTopLeft,
        Buttons::SamplerTopRight => ColourTargets::SamplerTopRight,
        Buttons::SamplerBottomLeft => ColourTargets::SamplerBottomLeft,
        Buttons::SamplerBottomRight => ColourTargets::SamplerBottomRight,
        Buttons::SamplerClear => ColourTargets::SamplerClear,
    }
}

fn map_fader_to_colour_target(fader: FaderName) -> ColourTargets {
    match fader {
        FaderName::A => ColourTargets::FadeMeter1,
        FaderName::B => ColourTargets::FadeMeter2,
        FaderName::C => ColourTargets::FadeMeter3,
        FaderName::D => ColourTargets::FadeMeter4,
    }
}

fn map_sample_bank_to_colour_target(bank: SampleBank) -> ColourTargets {
    match bank {
        SampleBank::A => ColourTargets::SamplerSelectA,
        SampleBank::B => ColourTargets::SamplerSelectB,
        SampleBank::C => ColourTargets::SamplerSelectC,
    }
}

fn get_profile_colour_map(profile: &ProfileSettings, colour_target: ColourTargets) -> &ColourMap {
    match colour_target {
        ColourTargets::Fader1Mute => profile.mute_button(Faders::A).colour_map(),
        ColourTargets::Fader2Mute => profile.mute_button(Faders::B).colour_map(),
        ColourTargets::Fader3Mute => profile.mute_button(Faders::C).colour_map(),
        ColourTargets::Fader4Mute => profile.mute_button(Faders::D).colour_map(),
        ColourTargets::Bleep => profile.simple_element(SimpleElements::Swear).colour_map(),
        ColourTargets::MicrophoneMute => profile.mute_chat().colour_map(),
        ColourTargets::EffectSelect1 => profile.effects(Preset::Preset1).colour_map(),
        ColourTargets::EffectSelect2 => profile.effects(Preset::Preset2).colour_map(),
        ColourTargets::EffectSelect3 => profile.effects(Preset::Preset3).colour_map(),
        ColourTargets::EffectSelect4 => profile.effects(Preset::Preset4).colour_map(),
        ColourTargets::EffectSelect5 => profile.effects(Preset::Preset5).colour_map(),
        ColourTargets::EffectSelect6 => profile.effects(Preset::Preset6).colour_map(),
        ColourTargets::EffectFx => profile.simple_element(SimpleElements::FxClear).colour_map(),
        ColourTargets::EffectMegaphone => profile.megaphone_effect().colour_map(),
        ColourTargets::EffectRobot => profile.robot_effect().colour_map(),
        ColourTargets::EffectHardTune => profile.hardtune_effect().colour_map(),
        ColourTargets::SamplerSelectA => profile
            .simple_element(SimpleElements::SampleBankA)
            .colour_map(),
        ColourTargets::SamplerSelectB => profile
            .simple_element(SimpleElements::SampleBankB)
            .colour_map(),
        ColourTargets::SamplerSelectC => profile
            .simple_element(SimpleElements::SampleBankC)
            .colour_map(),
        ColourTargets::SamplerTopLeft => profile.sample_button(TopLeft).colour_map(),
        ColourTargets::SamplerTopRight => profile.sample_button(TopRight).colour_map(),
        ColourTargets::SamplerBottomLeft => profile.sample_button(BottomLeft).colour_map(),
        ColourTargets::SamplerBottomRight => profile.sample_button(BottomRight).colour_map(),
        ColourTargets::SamplerClear => profile.sample_button(Clear).colour_map(),
        ColourTargets::FadeMeter1 => profile.fader(Faders::A).colour_map(),
        ColourTargets::FadeMeter2 => profile.fader(Faders::B).colour_map(),
        ColourTargets::FadeMeter3 => profile.fader(Faders::C).colour_map(),
        ColourTargets::FadeMeter4 => profile.fader(Faders::D).colour_map(),
        ColourTargets::Scribble1 => profile.scribble(Faders::A).colour_map(),
        ColourTargets::Scribble2 => profile.scribble(Faders::B).colour_map(),
        ColourTargets::Scribble3 => profile.scribble(Faders::C).colour_map(),
        ColourTargets::Scribble4 => profile.scribble(Faders::D).colour_map(),
        ColourTargets::PitchEncoder => profile.pitch_encoder().colour_map(),
        ColourTargets::GenderEncoder => profile.gender_encoder().colour_map(),
        ColourTargets::ReverbEncoder => profile.reverb_encoder().colour_map(),
        ColourTargets::EchoEncoder => profile.echo_encoder().colour_map(),
        ColourTargets::LogoX => profile.simple_element(SimpleElements::LogoX).colour_map(),
        ColourTargets::InternalLight => profile.simple_element(SimpleElements::LogoX).colour_map(),
    }
}

fn get_profile_colour_map_mut(
    profile: &mut ProfileSettings,
    colour_target: ColourTargets,
) -> &mut ColourMap {
    match colour_target {
        ColourTargets::Fader1Mute => profile.mute_button_mut(Faders::A).colour_map_mut(),
        ColourTargets::Fader2Mute => profile.mute_button_mut(Faders::B).colour_map_mut(),
        ColourTargets::Fader3Mute => profile.mute_button_mut(Faders::C).colour_map_mut(),
        ColourTargets::Fader4Mute => profile.mute_button_mut(Faders::D).colour_map_mut(),
        ColourTargets::Bleep => profile
            .simple_element_mut(SimpleElements::Swear)
            .colour_map_mut(),
        ColourTargets::MicrophoneMute => profile.mute_chat_mut().colour_map_mut(),
        ColourTargets::EffectSelect1 => profile.effects_mut(Preset::Preset1).colour_map_mut(),
        ColourTargets::EffectSelect2 => profile.effects_mut(Preset::Preset2).colour_map_mut(),
        ColourTargets::EffectSelect3 => profile.effects_mut(Preset::Preset3).colour_map_mut(),
        ColourTargets::EffectSelect4 => profile.effects_mut(Preset::Preset4).colour_map_mut(),
        ColourTargets::EffectSelect5 => profile.effects_mut(Preset::Preset5).colour_map_mut(),
        ColourTargets::EffectSelect6 => profile.effects_mut(Preset::Preset6).colour_map_mut(),
        ColourTargets::EffectFx => profile
            .simple_element_mut(SimpleElements::FxClear)
            .colour_map_mut(),
        ColourTargets::EffectMegaphone => profile.megaphone_effect_mut().colour_map_mut(),
        ColourTargets::EffectRobot => profile.robot_effect_mut().colour_map_mut(),
        ColourTargets::EffectHardTune => profile.hardtune_effect_mut().colour_map_mut(),
        ColourTargets::SamplerSelectA => profile
            .simple_element_mut(SimpleElements::SampleBankA)
            .colour_map_mut(),
        ColourTargets::SamplerSelectB => profile
            .simple_element_mut(SimpleElements::SampleBankB)
            .colour_map_mut(),
        ColourTargets::SamplerSelectC => profile
            .simple_element_mut(SimpleElements::SampleBankC)
            .colour_map_mut(),
        ColourTargets::SamplerTopLeft => profile.sample_button_mut(TopLeft).colour_map_mut(),
        ColourTargets::SamplerTopRight => profile.sample_button_mut(TopRight).colour_map_mut(),
        ColourTargets::SamplerBottomLeft => profile.sample_button_mut(BottomLeft).colour_map_mut(),
        ColourTargets::SamplerBottomRight => {
            profile.sample_button_mut(BottomRight).colour_map_mut()
        }
        ColourTargets::SamplerClear => profile.sample_button_mut(Clear).colour_map_mut(),
        ColourTargets::FadeMeter1 => profile.fader_mut(Faders::A).colour_map_mut(),
        ColourTargets::FadeMeter2 => profile.fader_mut(Faders::B).colour_map_mut(),
        ColourTargets::FadeMeter3 => profile.fader_mut(Faders::C).colour_map_mut(),
        ColourTargets::FadeMeter4 => profile.fader_mut(Faders::D).colour_map_mut(),
        ColourTargets::Scribble1 => profile.scribble_mut(Faders::A).colour_map_mut(),
        ColourTargets::Scribble2 => profile.scribble_mut(Faders::B).colour_map_mut(),
        ColourTargets::Scribble3 => profile.scribble_mut(Faders::C).colour_map_mut(),
        ColourTargets::Scribble4 => profile.scribble_mut(Faders::D).colour_map_mut(),
        ColourTargets::PitchEncoder => profile.pitch_encoder_mut().colour_map_mut(),
        ColourTargets::GenderEncoder => profile.gender_encoder_mut().colour_map_mut(),
        ColourTargets::ReverbEncoder => profile.reverb_encoder_mut().colour_map_mut(),
        ColourTargets::EchoEncoder => profile.echo_encoder_mut().colour_map_mut(),
        ColourTargets::LogoX => profile
            .simple_element_mut(SimpleElements::LogoX)
            .colour_map_mut(),
        ColourTargets::InternalLight => profile
            .simple_element_mut(SimpleElements::LogoX)
            .colour_map_mut(),
    }
}

pub fn standard_to_colour_target(target: ButtonColourTargets) -> ColourTargets {
    match target {
        ButtonColourTargets::Fader1Mute => ColourTargets::Fader1Mute,
        ButtonColourTargets::Fader2Mute => ColourTargets::Fader2Mute,
        ButtonColourTargets::Fader3Mute => ColourTargets::Fader3Mute,
        ButtonColourTargets::Fader4Mute => ColourTargets::Fader4Mute,
        ButtonColourTargets::Bleep => ColourTargets::Bleep,
        ButtonColourTargets::Cough => ColourTargets::MicrophoneMute,
        ButtonColourTargets::EffectSelect1 => ColourTargets::EffectSelect1,
        ButtonColourTargets::EffectSelect2 => ColourTargets::EffectSelect2,
        ButtonColourTargets::EffectSelect3 => ColourTargets::EffectSelect3,
        ButtonColourTargets::EffectSelect4 => ColourTargets::EffectSelect4,
        ButtonColourTargets::EffectSelect5 => ColourTargets::EffectSelect5,
        ButtonColourTargets::EffectSelect6 => ColourTargets::EffectSelect6,
        ButtonColourTargets::EffectFx => ColourTargets::EffectFx,
        ButtonColourTargets::EffectMegaphone => ColourTargets::EffectMegaphone,
        ButtonColourTargets::EffectRobot => ColourTargets::EffectRobot,
        ButtonColourTargets::EffectHardTune => ColourTargets::EffectHardTune,
        ButtonColourTargets::SamplerSelectA => ColourTargets::SamplerSelectA,
        ButtonColourTargets::SamplerSelectB => ColourTargets::SamplerSelectB,
        ButtonColourTargets::SamplerSelectC => ColourTargets::SamplerSelectC,
        ButtonColourTargets::SamplerTopLeft => ColourTargets::SamplerTopLeft,
        ButtonColourTargets::SamplerTopRight => ColourTargets::SamplerTopRight,
        ButtonColourTargets::SamplerBottomLeft => ColourTargets::SamplerBottomLeft,
        ButtonColourTargets::SamplerBottomRight => ColourTargets::SamplerBottomRight,
        ButtonColourTargets::SamplerClear => ColourTargets::SamplerClear,
    }
}

pub fn get_mini_colour_targets() -> Vec<ButtonColourTargets> {
    vec![
        ButtonColourTargets::Fader1Mute,
        ButtonColourTargets::Fader2Mute,
        ButtonColourTargets::Fader3Mute,
        ButtonColourTargets::Fader4Mute,
        ButtonColourTargets::Bleep,
        ButtonColourTargets::Cough,
    ]
}

pub fn get_sampler_colour_targets() -> Vec<ButtonColourTargets> {
    vec![
        ButtonColourTargets::SamplerTopLeft,
        ButtonColourTargets::SamplerTopRight,
        ButtonColourTargets::SamplerBottomLeft,
        ButtonColourTargets::SamplerBottomRight,
        ButtonColourTargets::SamplerClear,
    ]
}

pub fn get_sampler_selector_colour_targets() -> Vec<ButtonColourTargets> {
    vec![
        ButtonColourTargets::SamplerSelectA,
        ButtonColourTargets::SamplerSelectB,
        ButtonColourTargets::SamplerSelectC,
    ]
}

pub fn standard_to_profile_simple_colour(target: SimpleColourTargets) -> ColourTargets {
    match target {
        // This is technically incorrect, the Global doesn't have a matching Colour Target.
        SimpleColourTargets::Global => ColourTargets::InternalLight,
        SimpleColourTargets::Accent => ColourTargets::LogoX,
        SimpleColourTargets::Scribble1 => ColourTargets::Scribble1,
        SimpleColourTargets::Scribble2 => ColourTargets::Scribble2,
        SimpleColourTargets::Scribble3 => ColourTargets::Scribble3,
        SimpleColourTargets::Scribble4 => ColourTargets::Scribble4,
    }
}

pub fn standard_to_profile_encoder_colour(target: EncoderColourTargets) -> ColourTargets {
    match target {
        EncoderColourTargets::Reverb => ColourTargets::ReverbEncoder,
        EncoderColourTargets::Pitch => ColourTargets::PitchEncoder,
        EncoderColourTargets::Echo => ColourTargets::EchoEncoder,
        EncoderColourTargets::Gender => ColourTargets::GenderEncoder,
    }
}

pub fn standard_to_sample_colour(target: SamplerColourTargets) -> ColourTargets {
    match target {
        SamplerColourTargets::SamplerSelectA => ColourTargets::SamplerSelectA,
        SamplerColourTargets::SamplerSelectB => ColourTargets::SamplerSelectB,
        SamplerColourTargets::SamplerSelectC => ColourTargets::SamplerSelectC,
    }
}

pub fn standard_sample_colour_to_profile_bank(target: SamplerColourTargets) -> SampleBank {
    match target {
        SamplerColourTargets::SamplerSelectA => SampleBank::A,
        SamplerColourTargets::SamplerSelectB => SampleBank::B,
        SamplerColourTargets::SamplerSelectC => SampleBank::C,
    }
}

pub fn get_scribble_colour_targets() -> Vec<SimpleColourTargets> {
    vec![
        SimpleColourTargets::Scribble1,
        SimpleColourTargets::Scribble2,
        SimpleColourTargets::Scribble3,
        SimpleColourTargets::Scribble4,
    ]
}

#[allow(dead_code)]
fn standard_to_profile_reverb_style(style: goxlr_types::ReverbStyle) -> ReverbStyle {
    match style {
        goxlr_types::ReverbStyle::Library => ReverbStyle::Library,
        goxlr_types::ReverbStyle::DarkBloom => ReverbStyle::DarkBloom,
        goxlr_types::ReverbStyle::MusicClub => ReverbStyle::MusicClub,
        goxlr_types::ReverbStyle::RealPlate => ReverbStyle::RealPlate,
        goxlr_types::ReverbStyle::Chapel => ReverbStyle::Chapel,
        goxlr_types::ReverbStyle::HockeyArena => ReverbStyle::HockeyArena,
    }
}

fn profile_to_standard_reverb_style(style: &ReverbStyle) -> goxlr_types::ReverbStyle {
    match style {
        ReverbStyle::Library => goxlr_types::ReverbStyle::Library,
        ReverbStyle::DarkBloom => goxlr_types::ReverbStyle::DarkBloom,
        ReverbStyle::MusicClub => goxlr_types::ReverbStyle::MusicClub,
        ReverbStyle::RealPlate => goxlr_types::ReverbStyle::RealPlate,
        ReverbStyle::Chapel => goxlr_types::ReverbStyle::Chapel,
        ReverbStyle::HockeyArena => goxlr_types::ReverbStyle::HockeyArena,
    }
}

#[allow(dead_code)]
fn standard_to_profile_echo_style(style: goxlr_types::EchoStyle) -> EchoStyle {
    match style {
        goxlr_types::EchoStyle::Quarter => EchoStyle::Quarter,
        goxlr_types::EchoStyle::Eighth => EchoStyle::Eighth,
        goxlr_types::EchoStyle::Triplet => EchoStyle::Triplet,
        goxlr_types::EchoStyle::PingPong => EchoStyle::PingPong,
        goxlr_types::EchoStyle::ClassicSlap => EchoStyle::ClassicSlap,
        goxlr_types::EchoStyle::MultiTap => EchoStyle::MultiTap,
    }
}

fn profile_to_standard_echo_style(style: &EchoStyle) -> goxlr_types::EchoStyle {
    match style {
        EchoStyle::Quarter => goxlr_types::EchoStyle::Quarter,
        EchoStyle::Eighth => goxlr_types::EchoStyle::Eighth,
        EchoStyle::Triplet => goxlr_types::EchoStyle::Triplet,
        EchoStyle::PingPong => goxlr_types::EchoStyle::PingPong,
        EchoStyle::ClassicSlap => goxlr_types::EchoStyle::ClassicSlap,
        EchoStyle::MultiTap => goxlr_types::EchoStyle::MultiTap,
    }
}

#[allow(dead_code)]
fn standard_to_profile_pitch_style(style: goxlr_types::PitchStyle) -> PitchStyle {
    match style {
        goxlr_types::PitchStyle::Narrow => PitchStyle::Narrow,
        goxlr_types::PitchStyle::Wide => PitchStyle::Wide,
    }
}

fn profile_to_standard_pitch_style(style: &PitchStyle) -> goxlr_types::PitchStyle {
    match style {
        PitchStyle::Narrow => goxlr_types::PitchStyle::Narrow,
        PitchStyle::Wide => goxlr_types::PitchStyle::Wide,
    }
}

#[allow(dead_code)]
fn standard_to_profile_gender_style(style: goxlr_types::GenderStyle) -> GenderStyle {
    match style {
        goxlr_types::GenderStyle::Narrow => GenderStyle::Narrow,
        goxlr_types::GenderStyle::Medium => GenderStyle::Medium,
        goxlr_types::GenderStyle::Wide => GenderStyle::Wide,
    }
}

fn profile_to_standard_gender_style(style: &GenderStyle) -> goxlr_types::GenderStyle {
    match style {
        GenderStyle::Narrow => goxlr_types::GenderStyle::Narrow,
        GenderStyle::Medium => goxlr_types::GenderStyle::Medium,
        GenderStyle::Wide => goxlr_types::GenderStyle::Wide,
    }
}

#[allow(dead_code)]
fn standard_to_profile_megaphone_style(style: goxlr_types::MegaphoneStyle) -> MegaphoneStyle {
    match style {
        goxlr_types::MegaphoneStyle::Megaphone => MegaphoneStyle::Megaphone,
        goxlr_types::MegaphoneStyle::Radio => MegaphoneStyle::Radio,
        goxlr_types::MegaphoneStyle::OnThePhone => MegaphoneStyle::OnThePhone,
        goxlr_types::MegaphoneStyle::Overdrive => MegaphoneStyle::Overdrive,
        goxlr_types::MegaphoneStyle::BuzzCutt => MegaphoneStyle::BuzzCutt,
        goxlr_types::MegaphoneStyle::Tweed => MegaphoneStyle::Tweed,
    }
}

fn profile_to_standard_megaphone_style(style: &MegaphoneStyle) -> goxlr_types::MegaphoneStyle {
    match style {
        MegaphoneStyle::Megaphone => goxlr_types::MegaphoneStyle::Megaphone,
        MegaphoneStyle::Radio => goxlr_types::MegaphoneStyle::Radio,
        MegaphoneStyle::OnThePhone => goxlr_types::MegaphoneStyle::OnThePhone,
        MegaphoneStyle::Overdrive => goxlr_types::MegaphoneStyle::Overdrive,
        MegaphoneStyle::BuzzCutt => goxlr_types::MegaphoneStyle::BuzzCutt,
        MegaphoneStyle::Tweed => goxlr_types::MegaphoneStyle::Tweed,
    }
}

#[allow(dead_code)]
fn standard_to_profile_robot_style(style: goxlr_types::RobotStyle) -> RobotStyle {
    match style {
        goxlr_types::RobotStyle::Robot1 => RobotStyle::Robot1,
        goxlr_types::RobotStyle::Robot2 => RobotStyle::Robot2,
        goxlr_types::RobotStyle::Robot3 => RobotStyle::Robot3,
    }
}

fn profile_to_standard_robot_style(style: &RobotStyle) -> goxlr_types::RobotStyle {
    match style {
        RobotStyle::Robot1 => goxlr_types::RobotStyle::Robot1,
        RobotStyle::Robot2 => goxlr_types::RobotStyle::Robot2,
        RobotStyle::Robot3 => goxlr_types::RobotStyle::Robot3,
    }
}

#[allow(dead_code)]
fn standard_to_profile_hard_tune_style(style: goxlr_types::HardTuneStyle) -> HardTuneStyle {
    match style {
        goxlr_types::HardTuneStyle::Natural => HardTuneStyle::Natural,
        goxlr_types::HardTuneStyle::Medium => HardTuneStyle::Medium,
        goxlr_types::HardTuneStyle::Hard => HardTuneStyle::Hard,
    }
}

fn profile_to_standard_hard_tune_style(style: &HardTuneStyle) -> goxlr_types::HardTuneStyle {
    match style {
        HardTuneStyle::Natural => goxlr_types::HardTuneStyle::Natural,
        HardTuneStyle::Medium => goxlr_types::HardTuneStyle::Medium,
        HardTuneStyle::Hard => goxlr_types::HardTuneStyle::Hard,
    }
}

fn standard_to_profile_hard_tune_source(source: goxlr_types::HardTuneSource) -> HardTuneSource {
    match source {
        goxlr_types::HardTuneSource::All => HardTuneSource::All,
        goxlr_types::HardTuneSource::Music => HardTuneSource::Music,
        goxlr_types::HardTuneSource::Game => HardTuneSource::Game,
        goxlr_types::HardTuneSource::LineIn => HardTuneSource::LineIn,
        goxlr_types::HardTuneSource::System => HardTuneSource::System,
    }
}

fn profile_to_standard_hard_tune_source(source: &HardTuneSource) -> goxlr_types::HardTuneSource {
    match source {
        HardTuneSource::All => goxlr_types::HardTuneSource::All,
        HardTuneSource::Music => goxlr_types::HardTuneSource::Music,
        HardTuneSource::Game => goxlr_types::HardTuneSource::Game,
        HardTuneSource::LineIn => goxlr_types::HardTuneSource::LineIn,
        HardTuneSource::System => goxlr_types::HardTuneSource::System,
    }
}

pub fn version_newer_or_equal_to(version: &VersionNumber, comparison: VersionNumber) -> bool {
    match version.0.cmp(&comparison.0) {
        Ordering::Greater => return true,
        Ordering::Less => return false,
        Ordering::Equal => {}
    }

    match version.1.cmp(&comparison.1) {
        Ordering::Greater => return true,
        Ordering::Less => return false,
        Ordering::Equal => {}
    }

    match version.2.cmp(&comparison.2) {
        Ordering::Greater => return true,
        Ordering::Less => return false,
        Ordering::Equal => {}
    }

    if version.3 >= comparison.3 {
        return true;
    }

    false
}
