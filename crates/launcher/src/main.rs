//! python3.15t launcher: command-line and environment parsing (CPython's rules),
//! then hands control to the runtime.

use rpy_runtime::modules::pymain::{MainOptions, RunMode};
use rpy_runtime::modules::sys_full::RtConfig;

const USAGE: &str = "usage: {prog} [option] ... [-c cmd | -m mod | file | -] [arg] ...\n";

const HELP: &str = "Options (and corresponding environment variables):
-b     : issue warnings about converting bytes/bytearray to str and comparing
         bytes/bytearray with str or bytes with int. (-bb: issue errors)
-B     : don't write .pyc files on import; also PYTHONDONTWRITEBYTECODE=x
-c cmd : program passed in as string (terminates option list)
-d     : turn on parser debugging output (for experts only, only works on
         debug builds); also PYTHONDEBUG=x
-E     : ignore PYTHON* environment variables (such as PYTHONPATH)
-h     : print this help message and exit (also -? or --help)
-i     : inspect interactively after running script; forces a prompt even
         if stdin does not appear to be a terminal; also PYTHONINSPECT=x
-I     : isolate Python from the user's environment (implies -E, -P and -s)
-m mod : run library module as a script (terminates option list)
-O     : remove assert and __debug__-dependent statements; add .opt-1 before
         .pyc extension; also PYTHONOPTIMIZE=x
-OO    : do -O changes and also discard docstrings; add .opt-2 before
         .pyc extension
-P     : don't prepend a potentially unsafe path to sys.path; also
         PYTHONSAFEPATH
-q     : don't print version and copyright messages on interactive startup
-s     : don't add user site directory to sys.path; also PYTHONNOUSERSITE=x
-S     : don't imply 'import site' on initialization
-u     : force the stdout and stderr streams to be unbuffered;
         this option has no effect on stdin; also PYTHONUNBUFFERED=x
-v     : verbose (trace import statements); also PYTHONVERBOSE=x
         can be supplied multiple times to increase verbosity
-V     : print the Python version number and exit (also --version)
         when given twice, print more information about the build
-W arg : warning control; arg is action:message:category:module:lineno
         also PYTHONWARNINGS=arg
-x     : skip first line of source, allowing use of non-Unix forms of #!cmd
-X opt : set implementation-specific option
--check-hash-based-pycs always|default|never:
         control how Python invalidates hash-based .pyc files
--help-env: print help about Python environment variables and exit
--help-xoptions: print help about implementation-specific -X options and exit
--help-all: print complete help information and exit

Arguments:
file   : program read from script file
-      : program read from stdin (default; interactive mode if a tty)
arg ...: arguments passed to program in sys.argv[1:]
";

const HELP_ENV: &str = "Environment variables that change behavior:
PYTHONASYNCIODEBUG: turn on asyncio debug mode
PYTHONBREAKPOINT: callable that breakpoint() calls (\"0\" disables it)
PYTHONCASEOK    : ignore case in 'import' statements (Windows only)
PYTHON_CONTEXT_AWARE_WARNINGS: warnings.catch_warnings() uses a context variable
PYTHON_CPU_COUNT: number that os.cpu_count() and os.process_cpu_count() report
PYTHONDEVMODE   : enable the development mode, like -X dev
PYTHONFAULTHANDLER: dump the Python traceback on fatal errors, like -X faulthandler
PYTHON_GIL      : 0 or 1: run without or with the global interpreter lock
PYTHONHASHSEED  : 'random' or an integer in [0; 4294967295] seeding the hashes
                  of str and bytes objects (0 disables the randomization)
PYTHONHOME      : alternative <prefix> directory (or <prefix>:<exec_prefix>)
PYTHONINTMAXSTRDIGITS: digit limit of int<->str conversions (0: no limit)
PYTHONIOENCODING: encoding[:errors] of the standard streams
PYTHON_LAZY_IMPORTS: 'all' or 'normal': which imports are lazy, like -X lazy_imports
PYTHONMALLOC    : memory allocator and debug hooks ('debug' adds the hooks)
PYTHONMALLOCSTATS: print allocator statistics at exit
PYTHONNODEBUGRANGES: leave column information out of code objects
PYTHONPATH      : ':'-separated directories put in front of the default module
                  search path (the result is sys.path)
PYTHONPLATLIBDIR: overrides sys.platlibdir
PYTHONPYCACHEPREFIX: root of a separate tree for the .pyc cache files
PYTHONSAFEPATH  : do not put a possibly unsafe directory in front of sys.path
PYTHONSTARTUP   : file run when an interactive session starts
PYTHON_THREAD_INHERIT_CONTEXT: new threads start with a copy of the creator's
                  contextvars context
PYTHON_TLBC     : 0 or 1: thread-local copies of specialised bytecode
PYTHONTRACEMALLOC: trace allocations with tracemalloc (value: frames kept)
PYTHONUSERBASE  : base directory of the user site-packages
PYTHONUTF8      : 0 or 1: UTF-8 mode, like -X utf8
PYTHONWARNDEFAULTENCODING: EncodingWarning when an encoding is left out
PYTHONWARNINGS  : warning filters, like -W

These variables have equivalent command-line options (see --help):
PYTHONDEBUG     : -d
PYTHONDONTWRITEBYTECODE: -B
PYTHONINSPECT   : -i
PYTHONNOUSERSITE: -s
PYTHONOPTIMIZE  : -O
PYTHONUNBUFFERED: -u
PYTHONVERBOSE   : -v
";

const HELP_X: &str = "The following implementation-specific options are available:
-X cpu_count=N|default: number that os.cpu_count() reports
-X dev : enable Python Development Mode
-X faulthandler: dump the Python traceback on fatal errors
-X frozen_modules=[on|off]: whether to use frozen modules
-X gil=[0|1]: enable (1) or disable (0) the GIL
-X importtime[=2]: show how long each import takes
-X int_max_str_digits=N: limit the size of int<->str conversions
-X lazy_imports=[all|normal]: control lazy imports
-X no_debug_ranges: don't include extra location information in code objects
-X pycache_prefix=PATH: write .pyc files to a parallel tree under PATH
-X tlbc=[0|1]: thread-local copies of specialised bytecode
-X tracemalloc[=N]: trace Python memory allocations
-X utf8[=0|1]: enable (1) or disable (0) UTF-8 mode
-X warn_default_encoding: enable opt-in EncodingWarning for 'encoding=None'
";

/// Whether --help output is colourised (PYTHON_COLORS, NO_COLOR, FORCE_COLOR, TERM, tty).
fn help_colorized() -> bool {
    let var = |n: &str| std::env::var(n).ok().filter(|v| !v.is_empty());
    match var("PYTHON_COLORS").as_deref() {
        Some("0") => return false,
        Some("1") => return true,
        _ => {}
    }
    if var("NO_COLOR").is_some() {
        return false;
    }
    if var("FORCE_COLOR").is_some() {
        return true;
    }
    if var("TERM").as_deref() == Some("dumb") {
        return false;
    }
    unsafe { libc::isatty(1) == 1 }
}

/// Help text with headings and option names highlighted when colours are on.
fn help_text(text: &str) -> String {
    if !help_colorized() {
        return text.to_string();
    }
    const BOLD: &str = "\x1b[1m";
    const OPT: &str = "\x1b[1;36m";
    const RESET: &str = "\x1b[0m";
    let mut out = String::with_capacity(text.len() * 2);
    for line in text.split_inclusive('\n') {
        if let Some(rest) = line.strip_prefix("usage:") {
            out.push_str(BOLD);
            out.push_str("usage:");
            out.push_str(RESET);
            out.push_str(rest);
        } else if line.starts_with('-') || line.starts_with("PYTHON") {
            let end = line.find(|c: char| c == ' ' || c == ':').unwrap_or(line.len());
            out.push_str(OPT);
            out.push_str(&line[..end]);
            out.push_str(RESET);
            out.push_str(&line[end..]);
        } else if line.ends_with(":\n") && !line.starts_with(' ') {
            out.push_str(BOLD);
            out.push_str(line.trim_end_matches('\n'));
            out.push_str(RESET);
            out.push('\n');
        } else {
            out.push_str(line);
        }
    }
    out
}

fn prog_name() -> String {
    std::env::args().next().unwrap_or_else(|| "python3".into())
}

fn usage_error(msg: &str) -> ! {
    eprint!("{}\n{}", msg, USAGE.replace("{prog}", &prog_name()));
    eprintln!("Try `python -h' for more information.");
    std::process::exit(2);
}

/// Lexically normalised absolute form of `p` (like os.path.abspath).
fn abspath(p: &std::path::Path) -> String {
    let base = if p.is_absolute() { std::path::PathBuf::new() } else { std::env::current_dir().unwrap_or_default() };
    let mut parts: Vec<String> = Vec::new();
    for c in base.join(p).components() {
        match c {
            std::path::Component::ParentDir => {
                parts.pop();
            }
            std::path::Component::Normal(s) => parts.push(s.to_string_lossy().into_owned()),
            _ => {}
        }
    }
    format!("/{}", parts.join("/"))
}

/// Read `<argv[0]>._pth` if present: (sys.path entries, `import site` requested).
fn underpth_paths() -> Option<(Vec<String>, bool)> {
    let exe = std::env::args().next()?;
    if !exe.contains('/') {
        return None;
    }
    let text = std::fs::read_to_string(format!("{}._pth", exe)).ok()?;
    let dir = std::path::Path::new(&exe).parent().map(|d| d.to_path_buf()).unwrap_or_default();
    let mut paths = Vec::new();
    let mut site = false;
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line == "import site" {
            site = true;
            continue;
        }
        paths.push(abspath(&dir.join(line)));
    }
    Some((paths, site))
}

fn fatal(msg: &str) -> ! {
    eprintln!("Fatal Python error: {}", msg);
    std::process::exit(1);
}

/// Standard descriptors (bit n = fd n) that were closed when the process started. Rust's
/// runtime reopens closed ones on /dev/null before `main`, so this is recorded by an ELF
/// constructor, which runs earlier; sys.stdin/stdout/stderr are None for these, as in
/// CPython.
static CLOSED_STD_FDS: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

extern "C" fn record_closed_std_fds() {
    for fd in 0..3 {
        if unsafe { libc::fcntl(fd, libc::F_GETFD) } == -1 {
            CLOSED_STD_FDS.fetch_or(1 << fd, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

#[used]
#[unsafe(link_section = ".init_array")]
static RECORD_CLOSED_STD_FDS: extern "C" fn() = record_closed_std_fds;

fn main() {
    // Prefer running the interpreter on the process's main thread (like CPython:
    // tools such as strace without -f, signal delivery and thread ids all see the
    // main thread).  Its stack may grow up to RLIMIT_STACK, so raise the soft limit
    // to the stack size the interpreter wants; only if that is not permitted fall
    // back to a big-stack thread (grind-lib).
    const WANT: libc::rlim_t = 256 << 20;
    let main_ok = unsafe {
        let mut rl: libc::rlimit = std::mem::zeroed();
        if libc::getrlimit(libc::RLIMIT_STACK, &mut rl) != 0 {
            false
        } else if rl.rlim_cur == libc::RLIM_INFINITY || rl.rlim_cur >= WANT {
            true
        } else if rl.rlim_max == libc::RLIM_INFINITY || rl.rlim_max >= WANT {
            rl.rlim_cur = WANT;
            libc::setrlimit(libc::RLIMIT_STACK, &rl) == 0
        } else {
            false
        }
    };
    if main_ok {
        let code = real_main();
        std::process::exit(code);
    }
    // Keep process-directed signals away from the OS main thread (which only waits):
    // block them here, and restore the original mask in the interpreter thread.
    let mut old: libc::sigset_t = unsafe { std::mem::zeroed() };
    unsafe {
        let mut all: libc::sigset_t = std::mem::zeroed();
        libc::sigfillset(&mut all);
        for s in [libc::SIGSEGV, libc::SIGBUS, libc::SIGFPE, libc::SIGILL, libc::SIGABRT, libc::SIGTRAP] {
            libc::sigdelset(&mut all, s);
        }
        libc::pthread_sigmask(libc::SIG_BLOCK, &all, &mut old);
    }
    struct Mask(libc::sigset_t);
    unsafe impl Send for Mask {}
    let m = Mask(old);
    // run on a big-stack thread so deep recursion is bounded by the recursion limit
    let child = std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(move || {
            let m = m;
            unsafe { libc::pthread_sigmask(libc::SIG_SETMASK, &m.0, std::ptr::null_mut()) };
            let code = real_main();
            std::process::exit(code);
        })
        .unwrap();
    let code = child.join().unwrap_or(1);
    std::process::exit(code);
}

fn env(cfg: &RtConfig, name: &str) -> Option<String> {
    if cfg.ignore_environment != 0 {
        return None;
    }
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

/// Value of a PYTHON* "level" variable: a positive integer, or 1 for any other non-empty value.
fn env_level(cfg: &RtConfig, name: &str) -> i32 {
    match env(cfg, name) {
        Some(v) => v.trim().parse::<i32>().ok().filter(|n| *n > 0).unwrap_or(1),
        None => 0,
    }
}

fn parse_int_max_str_digits(v: &str, from_env: bool) -> i32 {
    match v.parse::<i64>() {
        Ok(n) if n == 0 || (640..=i32::MAX as i64).contains(&n) => n as i32,
        _ => {
            if from_env {
                fatal("config_init_int_max_str_digits: PYTHONINTMAXSTRDIGITS: invalid limit; must be >= 640 or 0 for unlimited.");
            }
            fatal("config_init_int_max_str_digits: -X int_max_str_digits: invalid limit; must be >= 640 or 0 for unlimited.");
        }
    }
}

fn real_main() -> i32 {
    // Arguments are bytes on POSIX: options are parsed from a lossy UTF-8 view, while the
    // exact bytes travel to sys.argv / sys.orig_argv (decoded there with surrogateescape).
    let raw: Vec<Vec<u8>> = std::env::args_os().map(|a| std::os::unix::ffi::OsStringExt::into_vec(a)).collect();
    let args: Vec<String> = raw.iter().map(|b| String::from_utf8_lossy(b).into_owned()).collect();
    let any_undecodable = raw.iter().any(|b| std::str::from_utf8(b).is_err());
    let mut cfg = RtConfig { orig_argv: args.clone(), closed_std_fds: CLOSED_STD_FDS.load(std::sync::atomic::Ordering::Relaxed), ..Default::default() };
    if any_undecodable {
        cfg.orig_argv_bytes = raw.clone();
    }
    let mut cmd_undecodable: Option<Vec<u8>> = None;
    let mut cmd: Option<String> = None;
    let mut module: Option<String> = None;
    let mut warn_cli: Vec<String> = Vec::new();
    let mut version = 0;
    let mut skip_first = false;
    let mut i = 1;
    let mut stdin_dash = false;
    let mut no_site_user = false;
    let mut force_hash_random = false;
    'outer: while i < args.len() {
        let a = &args[i];
        if a == "--" {
            i += 1;
            break;
        }
        if a == "-" {
            stdin_dash = true;
            break;
        }
        if !a.starts_with('-') || a.len() < 2 {
            break;
        }
        if let Some(long) = a.strip_prefix("--") {
            match long {
                "help" => {
                    print!("{}", help_text(&format!("{}{}", USAGE.replace("{prog}", &prog_name()), HELP)));
                    std::process::exit(0);
                }
                "help-env" => {
                    print!("{}", help_text(HELP_ENV));
                    std::process::exit(0);
                }
                "help-xoptions" => {
                    print!("{}", help_text(HELP_X));
                    std::process::exit(0);
                }
                "help-all" => {
                    print!("{}", help_text(&format!("{}{}{}{}", USAGE.replace("{prog}", &prog_name()), HELP, HELP_ENV, HELP_X)));
                    std::process::exit(0);
                }
                "version" => {
                    version += 1;
                    i += 1;
                    continue;
                }
                "check-hash-based-pycs" => {
                    let v = args.get(i + 1).cloned().unwrap_or_else(|| usage_error("Argument expected for the --check-hash-based-pycs option"));
                    if !["always", "default", "never"].contains(&v.as_str()) {
                        usage_error("--check-hash-based-pycs must be one of 'default', 'always', or 'never'");
                    }
                    cfg.check_hash_based_pycs = v;
                    i += 2;
                    continue;
                }
                _ => usage_error(&format!("Unknown option: {}", a)),
            }
        }
        let chars: Vec<char> = a[1..].chars().collect();
        let mut j = 0;
        while j < chars.len() {
            let c = chars[j];
            j += 1;
            let rest: String = chars[j..].iter().collect();
            // options taking an argument
            if matches!(c, 'c' | 'm' | 'W' | 'X') {
                let val = if !rest.is_empty() {
                    rest
                } else {
                    i += 1;
                    match args.get(i) {
                        Some(v) => v.clone(),
                        None => usage_error(&format!("Argument expected for the -{} option", c)),
                    }
                };
                i += 1;
                match c {
                    'c' => {
                        if std::str::from_utf8(&raw[i - 1]).is_err() {
                            // the command itself (an attached `-cCMD` carries the option prefix)
                            let whole = &raw[i - 1];
                            let skip = if args[i - 1] != val { whole.iter().position(|&b| b == b'c').map_or(0, |p| p + 1) } else { 0 };
                            cmd_undecodable = Some(whole[skip..].to_vec());
                        }
                        cmd = Some(val);
                        break 'outer;
                    }
                    'm' => {
                        module = Some(val);
                        break 'outer;
                    }
                    'W' => warn_cli.push(val),
                    _ => cfg.xoptions.push(val),
                }
                continue 'outer;
            }
            match c {
                'b' => cfg.bytes_warning += 1,
                'B' => cfg.dont_write_bytecode = 1,
                'd' => cfg.debug += 1,
                'E' => cfg.ignore_environment = 1,
                'h' | '?' => {
                    print!("{}", help_text(&format!("{}{}", USAGE.replace("{prog}", &prog_name()), HELP)));
                    std::process::exit(0);
                }
                'i' => {
                    cfg.inspect += 1;
                    cfg.interactive += 1;
                }
                'I' => {
                    cfg.isolated = 1;
                    cfg.ignore_environment = 1;
                    cfg.safe_path = true;
                    no_site_user = true;
                }
                'O' => cfg.optimize += 1,
                'P' => cfg.safe_path = true,
                'q' => cfg.quiet = 1,
                'R' => force_hash_random = true,
                's' => no_site_user = true,
                'S' => cfg.no_site = 1,
                'u' => cfg.unbuffered = true,
                'v' => cfg.verbose += 1,
                'V' => version += 1,
                'x' => skip_first = true,
                'J' => {}
                _ => usage_error(&format!("Unknown option: -{}", c)),
            }
        }
        i += 1;
    }
    if version > 0 {
        if version >= 2 {
            println!("Python {} free-threading build (main, Nov  1 2026, 00:00:00) [rustc]", rpy_runtime::modules::sys_full::VERSION);
        } else {
            println!("Python {}", rpy_runtime::modules::sys_full::VERSION);
        }
        std::process::exit(0);
    }
    // environment
    if no_site_user || env(&cfg, "PYTHONNOUSERSITE").is_some() {
        cfg.no_user_site = 1;
    }
    // PYTHONDEBUG only switches parser debugging on (unlike -d, it has no levels)
    cfg.debug = cfg.debug.max(env_level(&cfg, "PYTHONDEBUG").min(1));
    if env(&cfg, "PYTHONINSPECT").is_some() {
        cfg.inspect = cfg.inspect.max(1);
    }
    cfg.optimize = cfg.optimize.max(env_level(&cfg, "PYTHONOPTIMIZE"));
    if env(&cfg, "PYTHONDONTWRITEBYTECODE").is_some() {
        cfg.dont_write_bytecode = 1;
    }
    cfg.verbose = cfg.verbose.max(env_level(&cfg, "PYTHONVERBOSE"));
    if env(&cfg, "PYTHONUNBUFFERED").is_some() {
        cfg.unbuffered = true;
    }
    if env(&cfg, "PYTHONSAFEPATH").is_some() {
        cfg.safe_path = true;
    }
    if env(&cfg, "PYTHONDEVMODE").is_some() {
        cfg.dev_mode = true;
    }
    if env(&cfg, "PYTHONWARNDEFAULTENCODING").is_some() {
        cfg.warn_default_encoding = 1;
    }
    if env(&cfg, "PYTHONFAULTHANDLER").is_some() {
        cfg.faulthandler = true;
    }
    if let Some(v) = env(&cfg, "PYTHONTRACEMALLOC") {
        cfg.tracemalloc = match v.parse::<i64>() {
            Ok(n) if (0..=65535).contains(&n) => n as i32,
            _ => fatal("PYTHONTRACEMALLOC: invalid number of frames"),
        };
    }
    if let Some(v) = env(&cfg, "PYTHONUTF8") {
        cfg.utf8_mode = if v == "0" { 0 } else { 1 };
    }
    if let Some(v) = env(&cfg, "PYTHONPYCACHEPREFIX") {
        cfg.pycache_prefix = Some(v);
    }
    if let Some(v) = env(&cfg, "PYTHONINTMAXSTRDIGITS") {
        cfg.int_max_str_digits = parse_int_max_str_digits(&v, true);
    }
    if let Some(v) = env(&cfg, "PYTHON_GIL") {
        match v.as_str() {
            "0" => cfg.gil = 0,
            "1" => cfg.gil = 1,
            _ => fatal("config_read_gil: PYTHON_GIL / -X gil must be \"0\" or \"1\""),
        }
    }
    if let Some(v) = env(&cfg, "PYTHON_LAZY_IMPORTS") {
        cfg.lazy_imports = match v.as_str() {
            "all" => 1,
            "normal" => 0,
            "" => -1,
            _ => fatal("PYTHON_LAZY_IMPORTS: invalid value; expected 'all' or 'normal'"),
        };
    }
    if let Some(v) = env(&cfg, "PYTHONPROFILEIMPORTTIME") {
        cfg.import_time = if v == "2" { 2 } else { 1 };
    }
    if let Some(v) = env(&cfg, "PYTHON_TLBC") {
        if v != "0" && v != "1" {
            fatal("config_read_tlbc: PYTHON_TLBC=N: N is missing or invalid");
        }
    }
    if let Some(v) = env(&cfg, "PYTHON_CPU_COUNT") {
        if v != "default" {
            match v.parse::<i32>() {
                Ok(n) if n >= 1 => cfg.cpu_count = n,
                _ => fatal("config_init_cpu_count: PYTHON_CPU_COUNT=n option: n is missing or an invalid number, n must be greater than 0"),
            }
        }
    }
    if let Some(v) = env(&cfg, "PYTHON_THREAD_INHERIT_CONTEXT") {
        cfg.thread_inherit_context = if v == "0" { 0 } else { 1 };
    }
    if let Some(v) = env(&cfg, "PYTHON_CONTEXT_AWARE_WARNINGS") {
        cfg.context_aware_warnings = if v == "0" { 0 } else { 1 };
    }
    // -X options
    for x in cfg.xoptions.clone() {
        let (k, v) = match x.find('=') {
            Some(p) => (&x[..p], Some(&x[p + 1..])),
            None => (x.as_str(), None),
        };
        match k {
            "dev" => cfg.dev_mode = true,
            "faulthandler" => cfg.faulthandler = true,
            "utf8" => cfg.utf8_mode = if v == Some("0") { 0 } else { 1 },
            "warn_default_encoding" => cfg.warn_default_encoding = 1,
            // `-X pycache_prefix` or an empty value: no prefix, whatever the environment says
            "pycache_prefix" => cfg.pycache_prefix = v.filter(|s| !s.is_empty()).map(|s| s.to_string()),
            "importtime" => {
                cfg.import_time = match v.map(|s| s.parse::<i64>()) {
                    None => 1,
                    Some(Ok(n)) if (0..=2).contains(&n) => n as i32,
                    Some(Ok(_)) => fatal("-X importtime: invalid number; expected 0, 1 or 2"),
                    // any other value (e.g. "true") just switches it on
                    Some(Err(_)) => 1,
                }
            }
            "tlbc" => match v {
                Some("0") | Some("1") => {}
                _ => fatal("config_read_tlbc: -X tlbc=n: n is missing or invalid"),
            },
            "cpu_count" => match v {
                Some("default") => cfg.cpu_count = -1,
                Some(s) => match s.parse::<i32>() {
                    Ok(n) if n >= 1 => cfg.cpu_count = n,
                    _ => fatal("config_init_cpu_count: -X cpu_count=n option: n is missing or an invalid number, n must be greater than 0"),
                },
                None => fatal("config_init_cpu_count: -X cpu_count=n option: n is missing or an invalid number, n must be greater than 0"),
            },
            "tracemalloc" => {
                cfg.tracemalloc = match v {
                    None => 1,
                    Some(s) => match s.parse::<i32>() {
                        Ok(n) if (1..=65535).contains(&n) => n,
                        _ => fatal("-X tracemalloc=NFRAME: invalid number of frames"),
                    },
                }
            }
            "int_max_str_digits" => match v {
                Some(s) => cfg.int_max_str_digits = parse_int_max_str_digits(s, false),
                None => fatal("config_init_int_max_str_digits: -X int_max_str_digits: invalid limit; must be >= 640 or 0 for unlimited."),
            },
            "gil" => match v {
                Some("0") => cfg.gil = 0,
                Some("1") => cfg.gil = 1,
                _ => fatal("config_read_gil: PYTHON_GIL / -X gil must be \"0\" or \"1\""),
            },
            "lazy_imports" => {
                cfg.lazy_imports = match v {
                    Some("all") => 1,
                    Some("normal") => 0,
                    _ => fatal("-X lazy_imports: invalid value; expected 'all' or 'normal'"),
                }
            }
            "thread_inherit_context" => cfg.thread_inherit_context = if v == Some("0") { 0 } else { 1 },
            "context_aware_warnings" => cfg.context_aware_warnings = if v == Some("0") { 0 } else { 1 },
            _ => {}
        }
    }
    if cfg.dev_mode {
        cfg.faulthandler = true;
    }
    cfg.bytes_warning = cfg.bytes_warning.min(2);
    // warnings: PYTHONWARNINGS first, then -b/-W (CPython ordering: env, then cmdline)
    let mut warn = Vec::new();
    if cfg.dev_mode {
        warn.push("default".to_string());
    }
    if let Some(w) = env(&cfg, "PYTHONWARNINGS") {
        warn.extend(w.split(',').filter(|s| !s.is_empty()).map(|s| s.trim().to_string()));
    }
    warn.extend(warn_cli);
    match cfg.bytes_warning {
        0 => {}
        1 => warn.push("default::BytesWarning".to_string()),
        _ => warn.push("error::BytesWarning".to_string()),
    }
    cfg.warnoptions = warn;
    let seed = match env(&cfg, "PYTHONHASHSEED") {
        Some(s) if s == "random" => None,
        Some(s) => match s.parse::<u64>() {
            Ok(n) if n <= 4294967295 => {
                if n == 0 {
                    cfg.hash_randomization = 0;
                }
                Some(n as u32)
            }
            _ => fatal("config_init_hash_seed: PYTHONHASHSEED must be \"random\" or an integer in range [0; 4294967295]"),
        },
        None => None,
    };
    if force_hash_random {
        cfg.hash_randomization = 1;
    }
    if let Some(b) = &cmd_undecodable {
        // the command must be valid source text: report the first undecodable byte the way
        // encoding the surrogateescape-decoded text back to UTF-8 fails
        let (pos, byte) = first_undecodable(b);
        eprintln!("Unable to decode the command from the command line:");
        eprintln!("UnicodeEncodeError: 'utf-8' codec can't encode character '\\udc{:02x}' in position {}: surrogates not allowed", byte, pos);
        return 1;
    }
    let rest: Vec<String> = args[i.min(args.len())..].to_vec();
    let rest_raw: Vec<Vec<u8>> = raw[i.min(raw.len())..].to_vec();
    let mut argv: Vec<String> = Vec::new();
    let mut argv_bytes: Vec<Vec<u8>> = Vec::new();
    let mode;
    if let Some(c) = &cmd {
        argv.push("-c".into());
        argv.extend(rest.iter().cloned());
        argv_bytes.push(b"-c".to_vec());
        argv_bytes.extend(rest_raw.iter().cloned());
        mode = RunMode::Command(c.clone());
    } else if let Some(m) = &module {
        argv.push("-m".into());
        argv.extend(rest.iter().cloned());
        argv_bytes.push(b"-m".to_vec());
        argv_bytes.extend(rest_raw.iter().cloned());
        mode = RunMode::Module(m.clone());
    } else if !rest.is_empty() && !stdin_dash {
        argv.extend(rest.iter().cloned());
        argv_bytes.extend(rest_raw.iter().cloned());
        mode = RunMode::Script(rest[0].clone());
    } else {
        if stdin_dash {
            argv.push("-".into());
            argv.extend(args[i + 1..].iter().cloned());
            argv_bytes.push(b"-".to_vec());
            argv_bytes.extend(raw[i + 1..].iter().cloned());
        } else {
            argv.push(String::new());
            argv_bytes.push(Vec::new());
        }
        let tty = unsafe { libc::isatty(0) } != 0;
        mode = if tty || cfg.inspect > 0 && !stdin_dash { RunMode::Repl } else { RunMode::Stdin };
    }
    if matches!(mode, RunMode::Repl) {
        cfg.interactive = cfg.interactive.max(1);
    }
    // `<executable>._pth`: fixed sys.path, no environment paths, site only on request
    let pth_paths = underpth_paths();
    if let Some((_, import_site)) = &pth_paths {
        cfg.safe_path = true;
        cfg.no_user_site = 1;
        if !import_site {
            cfg.no_site = 1;
        }
    }
    let inspect = cfg.inspect > 0;
    let quiet = cfg.quiet != 0;
    let safe_path = cfg.safe_path;
    let opt = cfg.optimize;
    if !any_undecodable {
        argv_bytes.clear();
    }
    let rcfg = rpy_runtime::Config { argv, argv_bytes, hash_seed: seed, optimize: opt, sys: cfg, pth_paths: pth_paths.map(|p| p.0) };
    rpy_bindings::register();
    if rpy_runtime::initialize(&rcfg).is_err() {
        eprintln!("Fatal Python error: initialization failed");
        rpy_runtime::print_exception_and_clear();
        return 1;
    }
    rpy_runtime::modules::pymain::run_main(MainOptions {
        mode,
        inspect,
        skip_first_line: skip_first,
        quiet,
        safe_path,
        program_name: prog_name(),
    })
}

/// (character position, byte) of the first byte that is not part of valid UTF-8 in `b`,
/// counting characters the way surrogateescape decoding produces them.
fn first_undecodable(b: &[u8]) -> (usize, u8) {
    match std::str::from_utf8(b) {
        Ok(_) => (0, 0),
        Err(e) => {
            let valid = e.valid_up_to();
            let chars = std::str::from_utf8(&b[..valid]).map_or(0, |s| s.chars().count());
            (chars, b[valid])
        }
    }
}
