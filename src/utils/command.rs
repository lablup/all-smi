use std::process::Command;

/// Create a command with platform dependend handling
///
/// On Windows, the command is created with the `CREATE_NO_WINDOW` flag to
/// prevent a console window from appearing. This is required when all-smi
/// is used as a library for GUI applications.
///
/// On non-Windows this function is equivalent to `std::process::Command`
pub fn new_command(command: &str) -> Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;

        // CREATE_NO_WINDOW constant as described in Windows API docs
        // https://learn.microsoft.com/en-us/windows/win32/procthread/process-creation-flags
        const CREATE_NO_WINDOW: u32 = 0x08000000;

        let mut cmd = Command::new(command);
        cmd.creation_flags(CREATE_NO_WINDOW);
        cmd
    }

    #[cfg(not(windows))]
    Command::new(command)
}
