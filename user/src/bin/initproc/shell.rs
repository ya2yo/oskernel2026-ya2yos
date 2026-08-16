//! A small interactive shell for the Ya2yOS console.
//!
//! The shell deliberately uses only the user ABI supplied by Ya2yOS.  That
//! keeps PID 1 usable on images that do not provide a standalone `/bin/sh`,
//! while external commands still come from the Linux-compatible rootfs.

use alloc::{
    string::{String, ToString},
    vec::Vec,
};
use core::str;
use user_lib::{
    chdir, close, dup2, execve, exit, fork, getcwd, openat, pipe, read, waitpid,
    waitpid_with_options_raw, write, OpenFlags, WNOHANG,
};

const AT_FDCWD: isize = -100;
const MAX_LINE: usize = 1024;
const FILE_MODE: u32 = 0o666;
const COMMAND_PATHS: &[&str] = &["/bin", "/usr/bin", "/sbin", "/usr/sbin", "/musl", "/glibc"];

#[derive(Debug)]
enum Token {
    Word(String),
    Pipe,
    Input,
    Output,
    Append,
    ErrorOutput,
    ErrorAppend,
    Background,
    Sequence,
}

#[derive(Clone, Copy)]
enum RedirectKind {
    Input,
    Output,
    Append,
    ErrorOutput,
    ErrorAppend,
}

struct Redirect {
    kind: RedirectKind,
    path: String,
}

struct Command {
    argv: Vec<String>,
    redirects: Vec<Redirect>,
}

impl Command {
    fn new() -> Self {
        Self {
            argv: Vec::new(),
            redirects: Vec::new(),
        }
    }
}

fn output(message: &str) {
    let _ = write(1, message.as_bytes(), message.len());
}

fn error(message: &str) {
    let _ = write(2, message.as_bytes(), message.len());
}

fn push_word(tokens: &mut Vec<Token>, word: &mut String) {
    if !word.is_empty() {
        tokens.push(Token::Word(core::mem::take(word)));
    }
}

fn tokenize(line: &[u8]) -> Result<Vec<Token>, &'static str> {
    let mut tokens = Vec::new();
    let mut word = String::new();
    let mut index = 0;
    let mut quote = 0u8;

    while index < line.len() {
        let byte = line[index];
        if quote != 0 {
            if byte == quote {
                quote = 0;
            } else if byte == b'\\' && quote == b'"' {
                index += 1;
                if index == line.len() {
                    return Err("trailing escape");
                }
                word.push(line[index] as char);
            } else {
                word.push(byte as char);
            }
            index += 1;
            continue;
        }

        match byte {
            b'\'' | b'"' => quote = byte,
            b'\\' => {
                index += 1;
                if index == line.len() {
                    return Err("trailing escape");
                }
                word.push(line[index] as char);
            }
            b' ' | b'\t' | b'\r' | b'\n' => push_word(&mut tokens, &mut word),
            b'#' if word.is_empty() => break,
            b'|' => {
                push_word(&mut tokens, &mut word);
                tokens.push(Token::Pipe);
            }
            b';' => {
                push_word(&mut tokens, &mut word);
                tokens.push(Token::Sequence);
            }
            b'&' => {
                push_word(&mut tokens, &mut word);
                tokens.push(Token::Background);
            }
            b'<' => {
                push_word(&mut tokens, &mut word);
                tokens.push(Token::Input);
            }
            b'>' => {
                push_word(&mut tokens, &mut word);
                if line.get(index + 1) == Some(&b'>') {
                    tokens.push(Token::Append);
                    index += 1;
                } else {
                    tokens.push(Token::Output);
                }
            }
            b'2' if word.is_empty() && line.get(index + 1) == Some(&b'>') => {
                if line.get(index + 2) == Some(&b'>') {
                    tokens.push(Token::ErrorAppend);
                    index += 2;
                } else {
                    tokens.push(Token::ErrorOutput);
                    index += 1;
                }
            }
            _ => word.push(byte as char),
        }
        index += 1;
    }

    if quote != 0 {
        return Err("unterminated quote");
    }
    push_word(&mut tokens, &mut word);
    Ok(tokens)
}

fn token_redirect_kind(token: &Token) -> Option<RedirectKind> {
    match token {
        Token::Input => Some(RedirectKind::Input),
        Token::Output => Some(RedirectKind::Output),
        Token::Append => Some(RedirectKind::Append),
        Token::ErrorOutput => Some(RedirectKind::ErrorOutput),
        Token::ErrorAppend => Some(RedirectKind::ErrorAppend),
        _ => None,
    }
}

fn parse_pipeline(tokens: &[Token]) -> Result<Vec<Command>, &'static str> {
    let mut commands = Vec::new();
    let mut command = Command::new();
    let mut index = 0;

    while index < tokens.len() {
        match &tokens[index] {
            Token::Word(word) => command.argv.push(word.clone()),
            Token::Pipe => {
                if command.argv.is_empty() {
                    return Err("empty command in pipeline");
                }
                commands.push(command);
                command = Command::new();
            }
            token @ (Token::Input
            | Token::Output
            | Token::Append
            | Token::ErrorOutput
            | Token::ErrorAppend) => {
                let Some(Token::Word(path)) = tokens.get(index + 1) else {
                    return Err("redirection needs a path");
                };
                command.redirects.push(Redirect {
                    kind: token_redirect_kind(token).unwrap(),
                    path: path.clone(),
                });
                index += 1;
            }
            Token::Background | Token::Sequence => return Err("unexpected command separator"),
        }
        index += 1;
    }

    if command.argv.is_empty() {
        return Err("empty command");
    }
    commands.push(command);
    Ok(commands)
}

fn nul_terminated(value: &str) -> String {
    let mut value = String::from(value);
    value.push('\0');
    value
}

fn path_exists(path: &str) -> bool {
    let path = nul_terminated(path);
    let fd = openat(AT_FDCWD, &path, OpenFlags::O_RDONLY, 0);
    if fd < 0 {
        return false;
    }
    let _ = close(fd as usize);
    true
}

fn resolve_command(command: &str) -> Option<String> {
    if command.contains('/') {
        return path_exists(command).then(|| String::from(command));
    }

    for directory in COMMAND_PATHS {
        let mut candidate = String::from(*directory);
        candidate.push('/');
        candidate.push_str(command);
        if path_exists(&candidate) {
            return Some(candidate);
        }
    }
    None
}

fn apply_redirects(redirects: &[Redirect]) -> Result<(), isize> {
    for redirect in redirects {
        let (target_fd, flags) = match redirect.kind {
            RedirectKind::Input => (0, OpenFlags::O_RDONLY),
            RedirectKind::Output => (
                1,
                OpenFlags::O_WRONLY | OpenFlags::O_CREATE | OpenFlags::O_TRUNC,
            ),
            RedirectKind::Append => (
                1,
                OpenFlags::O_WRONLY | OpenFlags::O_CREATE | OpenFlags::O_APPEND,
            ),
            RedirectKind::ErrorOutput => (
                2,
                OpenFlags::O_WRONLY | OpenFlags::O_CREATE | OpenFlags::O_TRUNC,
            ),
            RedirectKind::ErrorAppend => (
                2,
                OpenFlags::O_WRONLY | OpenFlags::O_CREATE | OpenFlags::O_APPEND,
            ),
        };
        let path = nul_terminated(&redirect.path);
        let fd = openat(AT_FDCWD, &path, flags, FILE_MODE);
        if fd < 0 {
            return Err(fd);
        }
        let result = dup2(fd as usize, target_fd, 0);
        let _ = close(fd as usize);
        if result < 0 {
            return Err(result);
        }
    }
    Ok(())
}

fn is_builtin(name: &str) -> bool {
    matches!(
        name,
        "cd" | "pwd" | "echo" | "true" | "false" | "help" | "exit"
    )
}

fn run_builtin_inner(command: &Command) -> i32 {
    match command.argv[0].as_str() {
        "cd" => {
            let target = command.argv.get(1).map(String::as_str).unwrap_or("/");
            let target = nul_terminated(target);
            if chdir(&target) < 0 {
                error("shell: cd: unable to change directory\n");
                1
            } else {
                0
            }
        }
        "pwd" => {
            print_cwd();
            0
        }
        "echo" => {
            let mut first = true;
            let mut newline = true;
            let mut start = 1;
            if command.argv.get(1).map(String::as_str) == Some("-n") {
                newline = false;
                start = 2;
            }
            for argument in command.argv.iter().skip(start) {
                if !first {
                    output(" ");
                }
                output(argument);
                first = false;
            }
            if newline {
                output("\n");
            }
            0
        }
        "true" => 0,
        "false" => 1,
        "help" => {
            output("builtins: cd pwd echo true false exit help\n");
            output("operators: | < > >> 2> 2>> ; &\n");
            0
        }
        "exit" => {
            let status = command
                .argv
                .get(1)
                .and_then(|value| value.parse::<i32>().ok())
                .unwrap_or(0);
            exit(status);
        }
        _ => 127,
    }
}

fn run_builtin(command: &Command) -> Option<i32> {
    if !command.redirects.is_empty() || command.argv.is_empty() || !is_builtin(&command.argv[0]) {
        return None;
    }
    Some(run_builtin_inner(command))
}

fn print_cwd() {
    let mut buffer = [0u8; 256];
    let buffer_len = buffer.len();
    let length = getcwd(&mut buffer, buffer_len);
    if length <= 0 {
        output("/\n");
        return;
    }
    let length = (length as usize).min(buffer.len());
    let end = buffer[..length]
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(length);
    if let Ok(path) = str::from_utf8(&buffer[..end]) {
        output(path);
        output("\n");
    }
}

fn print_prompt() {
    output("ya2yos:");
    let mut buffer = [0u8; 256];
    let buffer_len = buffer.len();
    let length = getcwd(&mut buffer, buffer_len);
    if length > 0 {
        let length = (length as usize).min(buffer.len());
        let end = buffer[..length]
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(length);
        if let Ok(path) = str::from_utf8(&buffer[..end]) {
            output(path);
        }
    } else {
        output("/");
    }
    output("$ ");
}

fn execute_pipeline(commands: Vec<Command>, background: bool) -> i32 {
    if commands.len() == 1 && !background {
        if let Some(status) = run_builtin(&commands[0]) {
            return status;
        }
    }

    let mut previous_read: Option<usize> = None;
    let mut children = Vec::new();
    for (index, command) in commands.iter().enumerate() {
        let resolved = match resolve_command(&command.argv[0]) {
            Some(path) => Some(path),
            None if is_builtin(&command.argv[0]) => None,
            None => {
                error("shell: command not found: ");
                error(&command.argv[0]);
                error("\n");
                if let Some(fd) = previous_read {
                    let _ = close(fd);
                }
                return 127;
            }
        };

        let mut next_pipe = [0u32; 2];
        let has_next = index + 1 < commands.len();
        if has_next && pipe(&mut next_pipe, 0) < 0 {
            error("shell: pipe failed\n");
            if let Some(fd) = previous_read {
                let _ = close(fd);
            }
            return 1;
        }

        let pid = fork();
        if pid < 0 {
            error("shell: fork failed\n");
            if let Some(fd) = previous_read {
                let _ = close(fd);
            }
            if has_next {
                let _ = close(next_pipe[0] as usize);
                let _ = close(next_pipe[1] as usize);
            }
            return 1;
        }
        if pid == 0 {
            if let Some(fd) = previous_read {
                if dup2(fd, 0, 0) < 0 {
                    exit(126);
                }
            }
            if has_next && dup2(next_pipe[1] as usize, 1, 0) < 0 {
                exit(126);
            }
            if let Some(fd) = previous_read {
                let _ = close(fd);
            }
            if has_next {
                let _ = close(next_pipe[0] as usize);
                let _ = close(next_pipe[1] as usize);
            }
            if apply_redirects(&command.redirects).is_err() {
                error("shell: redirection failed\n");
                exit(126);
            }
            if is_builtin(&command.argv[0]) {
                exit(run_builtin_inner(command));
            }

            let mut args = Vec::with_capacity(command.argv.len());
            args.push(nul_terminated(resolved.as_ref().unwrap()));
            for argument in command.argv.iter().skip(1) {
                args.push(nul_terminated(argument));
            }
            let argv: Vec<&str> = args.iter().map(String::as_str).collect();
            let result = execve(&argv);
            error("shell: exec failed\n");
            let _ = result;
            exit(127);
        }

        if let Some(fd) = previous_read {
            let _ = close(fd);
        }
        if has_next {
            let _ = close(next_pipe[1] as usize);
            previous_read = Some(next_pipe[0] as usize);
        } else {
            previous_read = None;
        }
        children.push(pid);
    }

    if background {
        output("[");
        output(&children[children.len() - 1].to_string());
        output("]\n");
        return 0;
    }

    let mut status = 0;
    for pid in children {
        let mut wait_status = 0;
        if waitpid(pid as usize, &mut wait_status) == pid {
            status = (wait_status >> 8) & 0xff;
        } else {
            status = 1;
        }
    }
    status
}

fn execute_tokens(tokens: Vec<Token>) -> i32 {
    let mut start = 0;
    let mut last_status = 0;
    for index in 0..=tokens.len() {
        let separator = match tokens.get(index) {
            Some(Token::Sequence) => Some(false),
            Some(Token::Background) => Some(true),
            None => Some(false),
            _ => None,
        };
        let Some(background) = separator else {
            continue;
        };
        if index != start {
            match parse_pipeline(&tokens[start..index]) {
                Ok(commands) => last_status = execute_pipeline(commands, background),
                Err(reason) => {
                    error("shell: syntax error: ");
                    error(reason);
                    error("\n");
                    last_status = 2;
                }
            }
        } else if index < tokens.len() {
            error("shell: syntax error: empty command\n");
            last_status = 2;
        }
        start = index + 1;
    }
    last_status
}

fn reap_background_jobs() {
    loop {
        let mut status = 0;
        let pid = waitpid_with_options_raw(-1, &mut status, WNOHANG);
        if pid <= 0 {
            return;
        }
        output("[");
        output(&pid.to_string());
        output("] done\n");
    }
}

/// Start an interactive POSIX-style command loop for PID 1.
pub fn run() -> i32 {
    let mut line = [0u8; MAX_LINE];
    let mut last_status = 0;
    loop {
        reap_background_jobs();
        print_prompt();
        let line_len = line.len();
        let count = read(0, &mut line, line_len);
        if count == 0 {
            output("\n");
            return last_status;
        }
        if count < 0 {
            error("shell: input read failed\n");
            return 1;
        }
        let count = (count as usize).min(line.len());
        match tokenize(&line[..count]) {
            Ok(tokens) if !tokens.is_empty() => last_status = execute_tokens(tokens),
            Ok(_) => {}
            Err(reason) => {
                error("shell: parse error: ");
                error(reason);
                error("\n");
                last_status = 2;
            }
        }
    }
}
