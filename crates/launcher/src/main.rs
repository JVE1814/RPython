//! python3.15t launcher: parses the command line and runs the runtime.

fn usage() -> ! {
    eprintln!("usage: python3 [-c cmd | -m mod | file | -] [arg] ...");
    std::process::exit(2);
}

fn main() {
    // run on a big-stack thread so deep recursion is bounded by the recursion limit
    let child = std::thread::Builder::new().stack_size(256 << 20).spawn(real_main).unwrap();
    let code = child.join().unwrap_or(1);
    std::process::exit(code);
}

fn real_main() -> i32 {
    let args: Vec<String> = std::env::args().collect();
    let mut i = 1;
    let mut optimize = 0;
    let mut cmd: Option<String> = None;
    let mut module: Option<String> = None;
    while i < args.len() {
        let a = &args[i];
        if a == "-c" {
            cmd = Some(args.get(i + 1).cloned().unwrap_or_else(|| usage()));
            i += 2;
            break;
        } else if a == "-m" {
            module = Some(args.get(i + 1).cloned().unwrap_or_else(|| usage()));
            i += 2;
            break;
        } else if a == "-O" {
            optimize = 1;
            i += 1;
        } else if a == "-OO" {
            optimize = 2;
            i += 1;
        } else if a.starts_with('-') && a.len() > 1 && a != "-" {
            // ignore unsupported flags for now (-u, -B, -E, -s, -I, -X ...)
            if a == "-X" || a == "-W" {
                i += 1;
            }
            i += 1;
        } else {
            break;
        }
    }
    let rest: Vec<String> = args[i..].to_vec();
    let mut argv: Vec<String> = Vec::new();
    if let Some(_) = &cmd {
        argv.push("-c".into());
        argv.extend(rest.iter().cloned());
    } else if let Some(m) = &module {
        argv.push(m.clone());
        argv.extend(rest.iter().cloned());
    } else if !rest.is_empty() {
        argv.extend(rest.iter().cloned());
    } else {
        argv.push(String::new());
    }
    let seed = std::env::var("PYTHONHASHSEED").ok().and_then(|s| if s == "random" { None } else { s.parse().ok() });
    let cfg = rpy_runtime::Config { argv: argv.clone(), hash_seed: seed, optimize };
    if rpy_runtime::initialize(&cfg).is_err() {
        eprintln!("Fatal Python error: initialization failed");
        rpy_runtime::print_exception_and_clear();
        return 1;
    }
    let path = rpy_runtime::modules::sys::get_sys_attr("path").unwrap();
    if let Some(c) = cmd {
        rpy_runtime::types::list::insert(&path, 0, rpy_runtime::types::str::new_str(""));
        return rpy_runtime::run_string(&c);
    }
    if let Some(m) = module {
        let cwd = std::env::current_dir().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
        rpy_runtime::types::list::insert(&path, 0, rpy_runtime::types::str::new_str(&cwd));
        return rpy_runtime::run_module(&m);
    }
    if !rest.is_empty() && rest[0] != "-" {
        let script = &rest[0];
        let dir = std::path::Path::new(script).parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
        let dir = std::fs::canonicalize(if dir.is_empty() { ".".to_string() } else { dir }).map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
        rpy_runtime::types::list::insert(&path, 0, rpy_runtime::types::str::new_str(&dir));
        return rpy_runtime::run_file(script);
    }
    let mut src = String::new();
    use std::io::Read;
    let _ = std::io::stdin().read_to_string(&mut src);
    rpy_runtime::types::list::insert(&path, 0, rpy_runtime::types::str::new_str(""));
    rpy_runtime::run_string(&src)
}
