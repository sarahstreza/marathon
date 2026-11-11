//! HDMI-CEC command-line options (feature `input-cec`): the TV's remote
//! controls Marathon, and switching the TV's input pauses and resumes it
//! (PLAN.md M7). Off unless `--cec` is given.

use marathon_api::Action;
use marathon_input_cec::keys::Colour;
use marathon_input_cec::{CecConfig, ColourButtons, parse_action};

/// The `--cec*` options.
#[derive(Debug, Clone, clap::Args)]
#[command(next_help_heading = "HDMI-CEC")]
pub struct CecArgs {
    /// Use the TV's remote over HDMI-CEC, and pause when the TV switches
    /// to another input or goes to standby.
    #[arg(long)]
    pub cec: bool,
    /// CEC adapter to use, e.g. /dev/cec0. Defaults to the first one
    /// found.
    #[arg(long, value_name = "PATH", requires = "cec")]
    pub cec_adapter: Option<String>,
    /// The TV's HDMI input Marathon is plugged into, if it cannot be
    /// detected.
    #[arg(long, value_name = "N", requires = "cec",
          value_parser = clap::value_parser!(u8).range(1..=15))]
    pub cec_hdmi_port: Option<u8>,
    /// The name the TV shows for Marathon's input (at most 14
    /// characters).
    #[arg(long, value_name = "NAME", default_value = "Marathon")]
    pub cec_name: String,
    /// Switch the TV to Marathon (turning it on) at startup. Off by
    /// default: Marathon never takes over the TV unasked.
    #[arg(long, requires = "cec")]
    pub cec_claim_on_start: bool,
    /// Stay paused when the TV switches back to Marathon's input, instead
    /// of resuming.
    #[arg(long, requires = "cec")]
    pub cec_no_resume: bool,
    /// What a colour button does, as COLOUR=ACTION: an action name, or an
    /// action's JSON (as in the HTTP API). For example
    /// `red=toggle_subtitles` or `blue={"action":"go_to_channel","number":1}`.
    /// Repeat for each button.
    #[arg(long = "cec-button", value_name = "COLOUR=ACTION", requires = "cec",
          value_parser = parse_button)]
    pub cec_buttons: Vec<(Colour, Action)>,
}

impl CecArgs {
    /// The CEC configuration, if `--cec` was given.
    pub fn config(&self) -> Option<CecConfig> {
        if !self.cec {
            return None;
        }
        let mut colours = ColourButtons::default();
        for (colour, action) in &self.cec_buttons {
            colours.set(*colour, Some(action.clone()));
        }
        Some(CecConfig {
            osd_name: self.cec_name.clone(),
            adapter: self.cec_adapter.clone(),
            hdmi_port: self.cec_hdmi_port,
            claim_on_start: self.cec_claim_on_start,
            resume_on_select: !self.cec_no_resume,
            colours,
            ..CecConfig::default()
        })
    }
}

fn parse_button(text: &str) -> Result<(Colour, Action), String> {
    let (colour, action) = text
        .split_once('=')
        .ok_or_else(|| "expected COLOUR=ACTION, e.g. red=toggle_subtitles".to_owned())?;
    let colour = Colour::ALL
        .into_iter()
        .find(|c| c.name().eq_ignore_ascii_case(colour.trim()))
        .ok_or_else(|| format!("{colour:?} is not red, green, yellow, or blue"))?;
    Ok((colour, parse_action(action)?))
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[derive(Debug, Parser)]
    struct Cli {
        #[command(flatten)]
        cec: CecArgs,
    }

    fn parse(args: &[&str]) -> Result<CecArgs, clap::Error> {
        Cli::try_parse_from(std::iter::once("x").chain(args.iter().copied())).map(|c| c.cec)
    }

    #[test]
    fn off_unless_asked_for() {
        assert_eq!(parse(&[]).unwrap().config(), None);
        // Options that only make sense with --cec need it.
        assert!(parse(&["--cec-adapter", "/dev/cec0"]).is_err());
        assert!(parse(&["--cec-claim-on-start"]).is_err());
    }

    #[test]
    fn defaults_are_polite() {
        let config = parse(&["--cec"]).unwrap().config().unwrap();
        assert_eq!(config, CecConfig::default());
        assert!(!config.claim_on_start);
        assert!(config.resume_on_select);
        assert_eq!(config.osd_name, "Marathon");
    }

    #[test]
    fn every_option_reaches_the_config() {
        let config = parse(&[
            "--cec",
            "--cec-adapter",
            "/dev/cec1",
            "--cec-hdmi-port",
            "4",
            "--cec-name",
            "Den TV",
            "--cec-claim-on-start",
            "--cec-no-resume",
            "--cec-button",
            "red=toggle_subtitles",
            "--cec-button",
            r#"Blue={"action":"go_to_channel","number":1}"#,
        ])
        .unwrap()
        .config()
        .unwrap();
        assert_eq!(config.adapter.as_deref(), Some("/dev/cec1"));
        assert_eq!(config.hdmi_port, Some(4));
        assert_eq!(config.osd_name, "Den TV");
        assert!(config.claim_on_start);
        assert!(!config.resume_on_select);
        assert_eq!(config.colours.red, Some(Action::ToggleSubtitles));
        assert_eq!(config.colours.blue, Some(Action::GoToChannel { number: 1 }));
        assert_eq!(config.colours.green, None);
    }

    #[test]
    fn bad_options_are_rejected() {
        for bad in [
            ["--cec-hdmi-port", "0"],
            ["--cec-hdmi-port", "16"],
            ["--cec-button", "purple=toggle_guide"],
            ["--cec-button", "red"],
            ["--cec-button", "red=self_destruct"],
        ] {
            let mut args = vec!["--cec"];
            args.extend(bad);
            assert!(parse(&args).is_err(), "{bad:?}");
        }
    }
}
