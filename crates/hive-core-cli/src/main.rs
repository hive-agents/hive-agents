use std::collections::VecDeque;
use std::env;

mod connect;
mod device;
mod host_key;
mod install;
mod status;
mod util;

fn main() {
    if let Err(err) = run() {
        eprintln!("error: {}", err);
        eprintln!("run 'hive-core --help' for usage");
        std::process::exit(1);
    }
}

fn run() -> util::Result<()> {
    let mut args: VecDeque<String> = env::args().skip(1).collect();
    let Some(cmd) = args.pop_front() else {
        print_usage();
        return Ok(());
    };

    match cmd.as_str() {
        "-h" | "--help" => {
            print_usage();
            Ok(())
        }
        "install" => {
            if wants_help(&args) {
                print_install_usage();
                return Ok(());
            }
            let opts = install::parse_args(&mut args)?;
            install::run(opts)
        }
        "status" => {
            if wants_help(&args) {
                print_status_usage();
                return Ok(());
            }
            let opts = status::parse_args(&mut args)?;
            status::run(opts)
        }
        "connect" => {
            if wants_help(&args) {
                print_connect_usage();
                return Ok(());
            }
            let opts = connect::parse_args(&mut args)?;
            connect::run(opts)
        }
        "device" => {
            if wants_help(&args) {
                print_device_usage();
                return Ok(());
            }
            run_device(args)
        }
        "fingerprint" => status::print_fingerprint(),
        _ => Err(util::err(format!("unknown command: {}", cmd))),
    }
}

fn wants_help(args: &VecDeque<String>) -> bool {
    args.iter().any(|arg| arg == "--help" || arg == "-h")
}

fn run_device(mut args: VecDeque<String>) -> util::Result<()> {
    let Some(sub) = args.pop_front() else {
        return Err(util::err("device requires a subcommand"));
    };

    match sub.as_str() {
        "add" => {
            let opts = device::parse_add_args(&mut args)?;
            device::add_device(opts)
        }
        "remove" => {
            let opts = device::parse_remove_args(&mut args)?;
            device::remove_device(opts)
        }
        "ls" | "list" => {
            let opts = device::parse_list_args(&mut args)?;
            device::list_devices(opts)
        }
        _ => Err(util::err(format!("unknown device subcommand: {}", sub))),
    }
}

fn print_usage() {
    println!("hive-core <command> [options]");
    println!("");
    println!("commands:");
    println!("  install     install or update hive-core services");
    println!("  status      show service status and connection hints");
    println!("  connect     export a Hive connection profile");
    println!("  device      manage authorized device keys");
    println!("  fingerprint print ssh host key fingerprint");
    println!("");
    println!("use 'hive-core <command> --help' for command options");
}

fn print_install_usage() {
    println!("hive-core install [options]");
    println!("");
    println!("options:");
    println!("  --root <PATH>           install root (default: /opt/hive-core)");
    println!("  --force                 overwrite compose.yml if present");
    println!("  --skip-up               skip docker compose up -d");
    println!("  --skip-format           skip juicefs format");
    println!("  --skip-connect          skip starting local mount");
    println!("  --skip-ssh-user         skip creating hivec authorized_keys");
    println!("  --replace-existing      replace existing device on pairing");
    println!("  --bucket <NAME>         bucket name (default: hive)");
    println!("  --volume <NAME>         juicefs volume name (default: hive)");
    println!("  --postgres-user <USER>  postgres user (default: juicefs)");
    println!("  --postgres-db <DB>      postgres db (default: juicefs_meta)");
    println!("  --pairing-binary <PATH> pairing binary path (skips build)");
    println!("  --wait-seconds <N>      wait for services before format (default: 60)");
    println!("  --b2-endpoint <URL>     Backblaze B2 S3 endpoint (disables local seaweedfs)");
    println!("  --b2-bucket <NAME>      Backblaze B2 bucket name");
    println!("  --b2-key-id <KEY>       Backblaze B2 key ID");
    println!("  --b2-application-key <KEY> Backblaze B2 application key");
}

fn print_status_usage() {
    println!("hive-core status [options]");
    println!("");
    println!("options:");
    println!("  --root <PATH>  install root (default: /opt/hive-core)");
}

fn print_device_usage() {
    println!("hive-core device <add|remove|ls> [options]");
    println!("");
    println!("device add options:");
    println!("  --name <NAME>                device name (no spaces)");
    println!("  --pubkey <KEY>               public key string");
    println!("  --pubkey-file <PATH>         public key file path");
    println!("  --authorized-keys <PATH>     authorized_keys path");
    println!("");
    println!("device remove options:");
    println!("  --name <NAME>                device name (no spaces)");
    println!("  --authorized-keys <PATH>     authorized_keys path");
    println!("");
    println!("device ls options:");
    println!("  --authorized-keys <PATH>     authorized_keys path");
}

fn print_connect_usage() {
    println!("hive-core connect [options]");
    println!("");
    println!("outputs a JSON envelope containing profile + otp + pair_url");
    println!("options:");
    println!("  --root <PATH>           install root (default: /opt/hive-core)");
    println!("  --host <HOST>           public ssh host (or set HIVE_HOST)");
    println!("  --port <PORT>           ssh port (default: 22)");
    println!("  --user <USER>           ssh user (default: hivec)");
    println!("  --display-name <NAME>   profile display name (default: Hive)");
    println!("  --out <PATH>            write profile json to a file");
    println!("  --pretty                pretty-print json output");
    println!("  --fingerprint <SHA256>  override host key fingerprint");
}
