use herdr_reviewr::actions::{self, NonUiRun};

fn main() -> anyhow::Result<()> {
    // `NonUiRun` decides both dispatch and the actions' read of which panes run the UI.
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    match NonUiRun::from_args(&args) {
        Some(NonUiRun::Action(name)) => std::process::exit(actions::run(&name)),
        None => herdr_reviewr::run(),
    }
}
