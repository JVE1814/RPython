"""Pure-Python has_key(): does the terminal send a code for a given key?

Used when the curses library does not provide has_key() itself.  A key is
supported when the terminfo capability that describes it is present.
"""

import _curses

# key constant name -> terminfo capability describing the key's code
_CAPABILITY_OF = {
    "KEY_A1": "ka1", "KEY_A3": "ka3", "KEY_B2": "kb2",
    "KEY_BACKSPACE": "kbs", "KEY_BEG": "kbeg", "KEY_BTAB": "kcbt",
    "KEY_C1": "kc1", "KEY_C3": "kc3", "KEY_CANCEL": "kcan",
    "KEY_CATAB": "ktbc", "KEY_CLEAR": "kclr", "KEY_CLOSE": "kclo",
    "KEY_COMMAND": "kcmd", "KEY_COPY": "kcpy", "KEY_CREATE": "kcrt",
    "KEY_CTAB": "kctab", "KEY_DC": "kdch1", "KEY_DL": "kdl1",
    "KEY_DOWN": "kcud1", "KEY_EIC": "krmir", "KEY_END": "kend",
    "KEY_ENTER": "kent", "KEY_EOL": "kel", "KEY_EOS": "ked",
    "KEY_EXIT": "kext", "KEY_FIND": "kfnd", "KEY_HELP": "khlp",
    "KEY_HOME": "khome", "KEY_IC": "kich1", "KEY_IL": "kil1",
    "KEY_LEFT": "kcub1", "KEY_LL": "kll", "KEY_MARK": "kmrk",
    "KEY_MESSAGE": "kmsg", "KEY_MOVE": "kmov", "KEY_NEXT": "knxt",
    "KEY_NPAGE": "knp", "KEY_OPEN": "kopn", "KEY_OPTIONS": "kopt",
    "KEY_PPAGE": "kpp", "KEY_PREVIOUS": "kprv", "KEY_PRINT": "kprt",
    "KEY_REDO": "krdo", "KEY_REFERENCE": "kref", "KEY_REFRESH": "krfr",
    "KEY_REPLACE": "krpl", "KEY_RESTART": "krst", "KEY_RESUME": "kres",
    "KEY_RIGHT": "kcuf1", "KEY_SAVE": "ksav", "KEY_SBEG": "kBEG",
    "KEY_SCANCEL": "kCAN", "KEY_SCOMMAND": "kCMD", "KEY_SCOPY": "kCPY",
    "KEY_SCREATE": "kCRT", "KEY_SDC": "kDC", "KEY_SDL": "kDL",
    "KEY_SELECT": "kslt", "KEY_SEND": "kEND", "KEY_SEOL": "kEOL",
    "KEY_SEXIT": "kEXT", "KEY_SF": "kind", "KEY_SFIND": "kFND",
    "KEY_SHELP": "kHLP", "KEY_SHOME": "kHOM", "KEY_SIC": "kIC",
    "KEY_SLEFT": "kLFT", "KEY_SMESSAGE": "kMSG", "KEY_SMOVE": "kMOV",
    "KEY_SNEXT": "kNXT", "KEY_SOPTIONS": "kOPT", "KEY_SPREVIOUS": "kPRV",
    "KEY_SPRINT": "kPRT", "KEY_SR": "kri", "KEY_SREDO": "kRDO",
    "KEY_SREPLACE": "kRPL", "KEY_SRIGHT": "kRIT", "KEY_SRSUME": "kRES",
    "KEY_SSAVE": "kSAV", "KEY_SSUSPEND": "kSPD", "KEY_STAB": "khts",
    "KEY_SUNDO": "kUND", "KEY_SUSPEND": "kspd", "KEY_UNDO": "kund",
    "KEY_UP": "kcuu1",
}

_capabilities = {}
for _name, _cap in _CAPABILITY_OF.items():
    _code = getattr(_curses, _name, None)
    if _code is not None:
        _capabilities[_code] = _cap
for _n in range(64):
    _capabilities[_curses.KEY_F0 + _n] = "kf%d" % _n
del _name, _cap, _code, _n


def has_key(ch):
    """Return True if the current terminal can send key *ch*."""
    if isinstance(ch, str):
        ch = ord(ch)
    cap = _capabilities.get(ch)
    if cap is None:
        return False
    return bool(_curses.tigetstr(cap))
