"""IDLE's configuration: default settings and the user's overrides.

There are four configuration types, each stored in its own file:

    main        general, editor, shell, indentation and theme/keys choice
    highlight   color themes
    keys        key sets (bindings of virtual events to key sequences)
    extensions  extensions and their options and bindings

Defaults come from config-TYPE.def in the idlelib directory; user settings
from config-TYPE.cfg in ~/.idlerc.  A user value overrides the default value
of the same option.  The shared IdleConf instance is `idleConf`.

ConfigChanges collects the changes made in the settings dialog and writes
them to the user files.
"""
import os
import sys

from configparser import ConfigParser, Error as _ParserError
from tkinter.font import Font
import idlelib


class InvalidConfigType(Exception): pass
class InvalidConfigSet(Exception): pass
class InvalidFgBg(Exception): pass
class InvalidTheme(Exception): pass


class IdleConfParser(ConfigParser):
    """A ConfigParser for one configuration file, with typed lookups."""

    def __init__(self, cfgFile, cfgDefaults=None):
        """*cfgFile* is the full path of the file (may be '')."""
        self.file = cfgFile
        ConfigParser.__init__(self, defaults=cfgDefaults, strict=False)

    def Get(self, section, option, type=None, default=None, raw=False):
        """Return the option's value converted by *type* ('bool', 'int' or
        None for str); *default* if the option is missing.

        A value that cannot be converted raises ValueError.
        """
        if not self.has_option(section, option):
            return default
        if type == 'bool':
            return self.getboolean(section, option)
        if type == 'int':
            return self.getint(section, option)
        return self.get(section, option, raw=raw)

    def GetOptionList(self, section):
        """Return the option names of *section* ([] if it is missing)."""
        if self.has_section(section):
            return self.options(section)
        return []

    def Load(self):
        """Read the file, if there is one."""
        if self.file:
            self.read(self.file)


class IdleUserConfParser(IdleConfParser):
    """A configuration file that the user's changes are written to."""

    def SetOption(self, section, option, value):
        """Set an option (creating the section); return True if anything
        changed."""
        if self.has_option(section, option):
            if self.get(section, option) == value:
                return False
            self.set(section, option, value)
            return True
        if not self.has_section(section):
            self.add_section(section)
        self.set(section, option, value)
        return True

    def RemoveOption(self, section, option):
        """Remove an option; return True if it existed."""
        if self.has_section(section):
            return self.remove_option(section, option)
        return False

    def AddSection(self, section):
        """Add *section* unless it already exists."""
        if not self.has_section(section):
            self.add_section(section)

    def RemoveEmptySections(self):
        """Delete the sections that have no options."""
        for section in self.sections():
            if not self.GetOptionList(section):
                self.remove_section(section)

    def IsEmpty(self):
        """Remove empty sections; return True if nothing is left."""
        self.RemoveEmptySections()
        return not self.sections()

    def Save(self):
        """Write the configuration to its file.

        An empty configuration removes the file.  Names starting with '#'
        (used while testing) are never written.
        """
        path = self.file
        if not path or path[0] == '#':
            return
        if self.IsEmpty():
            if os.path.exists(path):
                os.remove(path)
            return
        try:
            f = open(path, 'w')
        except OSError:
            # e.g. a read-only file left by another user: replace it
            os.unlink(path)
            f = open(path, 'w')
        with f:
            self.write(f)


# Events that are part of IDLE itself, with the bindings used when no key
# set provides one.
_CORE_KEY_DEFAULTS = {
    '<<copy>>': ['<Control-c>', '<Control-C>'],
    '<<cut>>': ['<Control-x>', '<Control-X>'],
    '<<paste>>': ['<Control-v>', '<Control-V>'],
    '<<beginning-of-line>>': ['<Control-a>', '<Home>'],
    '<<center-insert>>': ['<Control-l>'],
    '<<close-all-windows>>': ['<Control-q>'],
    '<<close-window>>': ['<Alt-F4>'],
    '<<do-nothing>>': ['<Control-x>'],
    '<<end-of-file>>': ['<Control-d>'],
    '<<python-docs>>': ['<F1>'],
    '<<python-context-help>>': ['<Shift-F1>'],
    '<<history-next>>': ['<Alt-n>'],
    '<<history-previous>>': ['<Alt-p>'],
    '<<interrupt-execution>>': ['<Control-c>'],
    '<<view-restart>>': ['<F6>'],
    '<<restart-shell>>': ['<Control-F6>'],
    '<<open-class-browser>>': ['<Alt-c>'],
    '<<open-module>>': ['<Alt-m>'],
    '<<open-new-window>>': ['<Control-n>'],
    '<<open-window-from-file>>': ['<Control-o>'],
    '<<plain-newline-and-indent>>': ['<Control-j>'],
    '<<print-window>>': ['<Control-p>'],
    '<<redo>>': ['<Control-y>'],
    '<<remove-selection>>': ['<Escape>'],
    '<<save-copy-of-window-as-file>>': ['<Alt-Shift-S>'],
    '<<save-window-as-file>>': ['<Alt-s>'],
    '<<save-window>>': ['<Control-s>'],
    '<<select-all>>': ['<Alt-a>'],
    '<<toggle-auto-coloring>>': ['<Control-slash>'],
    '<<undo>>': ['<Control-z>'],
    '<<find-again>>': ['<Control-g>', '<F3>'],
    '<<find-in-files>>': ['<Alt-F3>'],
    '<<find-selection>>': ['<Control-F3>'],
    '<<find>>': ['<Control-f>'],
    '<<replace>>': ['<Control-h>'],
    '<<goto-line>>': ['<Alt-g>'],
    '<<smart-backspace>>': ['<Key-BackSpace>'],
    '<<newline-and-indent>>': ['<Key-Return>', '<Key-KP_Enter>'],
    '<<smart-indent>>': ['<Key-Tab>'],
    '<<indent-region>>': ['<Control-Key-bracketright>'],
    '<<dedent-region>>': ['<Control-Key-bracketleft>'],
    '<<comment-region>>': ['<Alt-Key-3>'],
    '<<uncomment-region>>': ['<Alt-Key-4>'],
    '<<tabify-region>>': ['<Alt-Key-5>'],
    '<<untabify-region>>': ['<Alt-Key-6>'],
    '<<toggle-tabs>>': ['<Alt-Key-t>'],
    '<<change-indentwidth>>': ['<Alt-Key-u>'],
    '<<del-word-left>>': ['<Control-Key-BackSpace>'],
    '<<del-word-right>>': ['<Control-Key-Delete>'],
    '<<force-open-completions>>': ['<Control-Key-space>'],
    '<<expand-word>>': ['<Alt-Key-slash>'],
    '<<force-open-calltip>>': ['<Control-Key-backslash>'],
    '<<flash-paren>>': ['<Control-Key-0>'],
    '<<format-paragraph>>': ['<Alt-Key-q>'],
    '<<run-module>>': ['<Key-F5>'],
    '<<run-custom>>': ['<Shift-Key-F5>'],
    '<<check-module>>': ['<Alt-Key-x>'],
    '<<zoom-height>>': ['<Alt-Key-2>'],
}

# Extensions that became built-in features; their sections remain in
# config-extensions.def only to hold their options.
_FORMER_EXTENSIONS = ('AutoComplete', 'CodeContext', 'FormatParagraph',
                      'ParenMatch')

# Elements of a highlighting theme (every one has -foreground and
# -background, except the cursor).
_THEME_ELEMENTS = ('normal', 'keyword', 'builtin', 'comment', 'string',
                   'definition', 'hilite', 'break', 'hit', 'error',
                   'context', 'linenumber', 'stdout', 'stderr', 'console')


class IdleConf:
    """Access to all IDLE configuration (defaults and user settings)."""

    def __init__(self, _utest=False):
        self.config_types = ('main', 'highlight', 'keys', 'extensions')
        self.defaultCfg = {}
        self.userCfg = {}
        self.cfg = {}
        # (path, exception) of user files that could not be read
        self.file_load_errors = []
        if not _utest:
            self.CreateConfigHandlers()
            self.LoadCfgFiles()

    def CreateConfigHandlers(self):
        """Create a parser for each default and user configuration file."""
        idledir = os.path.dirname(__file__)
        self.userdir = userdir = '' if idlelib.testing else self.GetUserCfgDir()
        for cfg_type in self.config_types:
            self.defaultCfg[cfg_type] = IdleConfParser(
                os.path.join(idledir, f'config-{cfg_type}.def'))
            self.userCfg[cfg_type] = IdleUserConfParser(
                os.path.join(userdir or '#', f'config-{cfg_type}.cfg'))

    def GetUserCfgDir(self):
        """Return the directory for the user's files (~/.idlerc), creating
        it if needed.  Exit if that is impossible."""
        home = os.path.expanduser('~')
        if home != '~' and not os.path.exists(home):
            if not idlelib.testing:
                _print_warning(f'\n Warning: os.path.expanduser("~") points to\n '
                               f'{home},\n but the path does not exist.')
            home = '~'
        if home == '~':
            # no usable home directory: fall back to the current one
            home = os.getcwd()
        userdir = os.path.join(home, '.idlerc')
        if not os.path.exists(userdir):
            try:
                os.mkdir(userdir)
            except OSError:
                if not idlelib.testing:
                    _print_warning(f'\n Warning: unable to create user config '
                                   f'directory\n{userdir}\n Check path and '
                                   f'permissions.\n Exiting!\n')
                raise SystemExit
        return userdir

    def GetOption(self, configType, section, option, default=None, type=None,
                  warn_on_default=True, raw=False):
        """Return an option value: the user's if set, else the default
        configuration's, else *default* (with a warning, unless
        *warn_on_default* is false).

        *type* is 'bool', 'int' or None; an unconvertible user value is
        reported and the default configuration is tried.
        """
        user = self.userCfg[configType]
        try:
            if user.has_option(section, option):
                return user.Get(section, option, type=type, raw=raw)
        except ValueError:
            bad = user.Get(section, option, raw=raw)
            _warn(f'\n Warning: config.py - IdleConf.GetOption -\n invalid '
                  f'{type!r} value for configuration option {option!r}\n'
                  f' from section {section!r}: {bad!r}',
                  configType, section, option)
        try:
            if self.defaultCfg[configType].has_option(section, option):
                return self.defaultCfg[configType].Get(
                    section, option, type=type, raw=raw)
        except ValueError:
            pass
        if warn_on_default:
            _warn(f'\n Warning: config.py - IdleConf.GetOption -\n problem '
                  f'retrieving configuration option {option!r}\n from section '
                  f'{section!r}.\n returning default value: {default!r}',
                  configType, section, option)
        return default

    def SetOption(self, configType, section, option, value):
        """Set an option in the user configuration."""
        self.userCfg[configType].SetOption(section, option, value)

    def GetSectionList(self, configSet, configType):
        """Return the sections of the 'default' or 'user' configuration of
        *configType*."""
        if configType not in self.config_types:
            raise InvalidConfigType('Invalid configType specified')
        if configSet == 'user':
            parser = self.userCfg[configType]
        elif configSet == 'default':
            parser = self.defaultCfg[configType]
        else:
            raise InvalidConfigSet('Invalid configSet specified')
        return parser.sections()

    def GetHighlight(self, theme, element):
        """Return {'foreground': color, 'background': color} for an
        element of *theme*; the cursor gets the normal background."""
        if self.defaultCfg['highlight'].has_section(theme):
            colors = self.GetThemeDict('default', theme)
        else:
            colors = self.GetThemeDict('user', theme)
        fore = colors[element + '-foreground']
        if element == 'cursor':
            element = 'normal'
        back = colors[element + '-background']
        return {"foreground": fore, "background": back}

    def GetThemeDict(self, type, themeName):
        """Return all colors of a 'default' or 'user' theme as a dict.

        Missing colors are reported and replaced by black on white.
        """
        if type == 'user':
            parser = self.userCfg['highlight']
        elif type == 'default':
            parser = self.defaultCfg['highlight']
        else:
            raise InvalidTheme('Invalid theme type specified')
        theme = {}
        for element in _THEME_ELEMENTS:
            theme[element + '-foreground'] = '#000000'
            theme[element + '-background'] = '#ffffff'
        theme['cursor-foreground'] = '#000000'
        for key in theme:
            if not parser.has_option(themeName, key):
                _warn(f'\n Warning: config.IdleConf.GetThemeDict -\n theme '
                      f'{themeName!r} has no value for {key!r};\n returning '
                      f'default value: {theme[key]!r}',
                      'highlight', themeName, key)
            theme[key] = parser.Get(themeName, key, default=theme[key])
        return theme

    def CurrentTheme(self):
        """Return the name of the theme in use."""
        return self.current_colors_and_keys('Theme')

    def CurrentKeys(self):
        """Return the name of the key set in use."""
        return self.current_colors_and_keys('Keys')

    def current_colors_and_keys(self, section):
        """Return the theme (section 'Theme') or key set ('Keys') in use.

        [section] default = True selects a built-in definition: name2 (if
        set) or name.  default = False selects a user definition by name.
        A name without a definition falls back to the built-in default.
        """
        cfgname = 'highlight' if section == 'Theme' else 'keys'
        builtin = self.GetOption('main', section, 'default', type='bool',
                                 default=True)
        name = ''
        if builtin:
            name = self.GetOption('main', section, 'name2', default='')
        if not name:
            name = self.GetOption('main', section, 'name', default='')
        if name:
            source = self.defaultCfg if builtin else self.userCfg
            if source[cfgname].has_section(name):
                return name
        return "IDLE Classic" if section == 'Theme' else self.default_keys()

    @staticmethod
    def default_keys():
        """Return the built-in key set for this platform."""
        if sys.platform[:3] == 'win':
            return 'IDLE Classic Windows'
        if sys.platform == 'darwin':
            return 'IDLE Classic OSX'
        return 'IDLE Modern Unix'

    def GetExtensions(self, active_only=True, editor_only=False,
                      shell_only=False):
        """Return the names of the extensions.

        With *active_only*, only enabled ones, optionally only those enabled
        for editor windows or for the shell.
        """
        names = self.RemoveKeyBindNames(self.GetSectionList('default', 'extensions'))
        for name in self.RemoveKeyBindNames(self.GetSectionList('user', 'extensions')):
            if name not in names:
                names.append(name)
        names = [n for n in names if n not in _FORMER_EXTENSIONS]
        if not active_only:
            return names
        active = []
        for name in names:
            if not self.GetOption('extensions', name, 'enable', default=True,
                                  type='bool'):
                continue
            if editor_only or shell_only:
                option = 'enable_editor' if editor_only else 'enable_shell'
                if not self.GetOption('extensions', name, option, default=True,
                                      type='bool', warn_on_default=False):
                    continue
            active.append(name)
        return active

    def RemoveKeyBindNames(self, extnNameList):
        """Drop the NAME_bindings and NAME_cfgBindings section names."""
        return [n for n in extnNameList
                if not n.endswith(('_bindings', '_cfgBindings'))]

    def GetExtnNameForEvent(self, virtualEvent):
        """Return the extension whose configurable bindings include the
        virtual event *virtualEvent* (name without <<>>), or None."""
        found = None
        event = '<<' + virtualEvent + '>>'
        for name in self.GetExtensions(active_only=0):
            if event in self.GetExtensionKeys(name):
                found = name
        return found

    def GetExtensionKeys(self, extensionName):
        """Return {event: bindings} for the configurable bindings of an
        extension, as bound in the current key set."""
        section = extensionName + '_cfgBindings'
        current = self.GetCurrentKeySet()
        keys = {}
        if self.defaultCfg['extensions'].has_section(section):
            for name in self.defaultCfg['extensions'].GetOptionList(section):
                event = '<<' + name + '>>'
                keys[event] = current[event]
        return keys

    def __GetRawExtensionKeys(self, extensionName):
        # The configurable bindings of an extension as configured.
        section = extensionName + '_cfgBindings'
        keys = {}
        if self.defaultCfg['extensions'].has_section(section):
            for name in self.defaultCfg['extensions'].GetOptionList(section):
                binding = self.GetOption('extensions', section, name, default='')
                keys['<<' + name + '>>'] = binding.split()
        return keys

    def GetExtensionBindings(self, extensionName):
        """Return {event: bindings} for all bindings of an extension:
        the configurable ones and the fixed NAME_bindings ones."""
        bindings = self.GetExtensionKeys(extensionName)
        section = extensionName + '_bindings'
        if self.defaultCfg['extensions'].has_section(section):
            for name in self.defaultCfg['extensions'].GetOptionList(section):
                binding = self.GetOption('extensions', section, name, default='')
                bindings['<<' + name + '>>'] = binding.split()
        return bindings

    def GetKeyBinding(self, keySetName, eventStr):
        """Return the key sequences bound to *eventStr* ('<<name>>') in a
        key set ([] if none)."""
        name = eventStr[2:-2]
        binding = self.GetOption('keys', keySetName, name, default='',
                                 warn_on_default=False)
        return binding.split()

    def GetCurrentKeySet(self):
        """Return the bindings of the current key set (with extension
        bindings), adapted to the platform."""
        result = self.GetKeySet(self.CurrentKeys())
        if sys.platform == "darwin":
            # Tk on macOS knows the Option modifier, not Alt.
            for event, keys in result.items():
                fixed = [k.replace('<Alt-', '<Option-') for k in keys]
                if fixed != keys:
                    result[event] = fixed
        return result

    def GetKeySet(self, keySetName):
        """Return {event: bindings} for a key set plus the bindings of the
        active extensions.  An extension binding that uses a key sequence
        already taken by IDLE itself is disabled ('')."""
        keyset = self.GetCoreKeys(keySetName)
        used = {seq for seqs in keyset.values() for seq in seqs}
        for name in self.GetExtensions(active_only=1):
            for event, binding in self.__GetRawExtensionKeys(name).items():
                if any(seq in used for seq in binding):
                    binding = ''
                keyset[event] = binding
        return keyset

    def IsCoreBinding(self, virtualEvent):
        """Return True if *virtualEvent* (name without <<>>) is one of
        IDLE's own events."""
        return ('<<' + virtualEvent + '>>') in self.GetCoreKeys()

    def GetCoreKeys(self, keySetName=None):
        """Return {event: bindings} for IDLE's own events, from the key set
        *keySetName* (or the built-in fallback bindings)."""
        bindings = {event: list(keys) for event, keys in _CORE_KEY_DEFAULTS.items()}
        if not keySetName:
            return bindings
        if not (self.userCfg['keys'].has_section(keySetName) or
                self.defaultCfg['keys'].has_section(keySetName)):
            _warn(f'\n Warning: config.py - IdleConf.GetCoreKeys -\n key set '
                  f'{keySetName!r} is not defined, using default bindings.',
                  'keys', keySetName)
            return bindings
        for event in bindings:
            binding = self.GetKeyBinding(keySetName, event)
            if binding:
                bindings[event] = binding
            else:
                _warn(f'\n Warning: config.py - IdleConf.GetCoreKeys -\n problem '
                      f'retrieving key binding for event {event!r}\n from key set '
                      f'{keySetName!r}.\n returning default value: '
                      f'{bindings[event]!r}', 'keys', keySetName, event)
        return bindings

    def GetExtraHelpSourceList(self, configSet):
        """Return [(menu item, path or URL, option)] for the additional help
        sources of the 'default' or 'user' configuration, ordered by option
        number.  Entries are "menu item;path" strings."""
        if configSet == 'user':
            parser = self.userCfg['main']
        elif configSet == 'default':
            parser = self.defaultCfg['main']
        else:
            raise InvalidConfigSet('Invalid configSet specified')
        sources = []
        for option in parser.GetOptionList('HelpFiles'):
            value = parser.Get('HelpFiles', option, default=';')
            parts = value.split(';')
            if len(parts) != 2 or not parts[0] or not parts[1]:
                continue
            sources.append((parts[0], parts[1], option))
        sources.sort(key=lambda source: int(source[2]))
        return sources

    def GetAllExtraHelpSourcesList(self):
        """Return the default and the user's additional help sources."""
        return (self.GetExtraHelpSourceList('default') +
                self.GetExtraHelpSourceList('user'))

    def GetFont(self, root, configType, section):
        """Return (family, size, weight) of the font configured in
        *section*.  TkFixedFont is resolved to the actual font."""
        family = self.GetOption(configType, section, 'font', default='courier')
        size = self.GetOption(configType, section, 'font-size', type='int',
                              default='10')
        bold = self.GetOption(configType, section, 'font-bold', default=0,
                              type='bool')
        if family == 'TkFixedFont':
            actual = Font.actual(Font(name='TkFixedFont', exists=True, root=root))
            family = actual['family']
            size = actual['size']
            if size <= 0:
                size = 10  # a size in pixels: use a sane point size
            bold = actual['weight'] == 'bold'
        return (family, size, 'bold' if bold else 'normal')

    def LoadCfgFiles(self):
        """Read all configuration files.

        A user file that cannot be parsed (or decoded) is renamed to
        NAME.bad, its settings are ignored and the error is recorded in
        file_load_errors.
        """
        for key in self.defaultCfg:
            self.defaultCfg[key].Load()
            user = self.userCfg[key]
            try:
                user.Load()
            except (_ParserError, UnicodeDecodeError) as err:
                self.file_load_errors.append((user.file, err))
                for section in user.sections():
                    user.remove_section(section)
                try:
                    os.replace(user.file, user.file + '.bad')
                except OSError:
                    pass

    def file_load_error_message(self):
        """Describe the unreadable user files (None if there are none)."""
        if not self.file_load_errors:
            return None
        lines = ["IDLE could not read these configuration files.  They were "
                 "renamed with the extension '.bad' and IDLE's default "
                 "settings are used instead:", ""]
        for path, err in self.file_load_errors:
            lines.append(f"{path}\n    {type(err).__name__}: {err}")
        return '\n'.join(lines)

    def SaveUserCfgFiles(self):
        """Write all user configuration files."""
        for key in self.userCfg:
            self.userCfg[key].Save()


def _print_warning(msg):
    try:
        print(msg, file=sys.stderr)
    except OSError:
        pass


_warned = set()


def _warn(msg, *key):
    """Print *msg* to stderr once per *key*."""
    key = (msg,) + key
    if key not in _warned:
        _print_warning(msg)
        _warned.add(key)


idleConf = IdleConf()


class ConfigChanges(dict):
    """Pending configuration changes: {config type: {section: {item:
    value}}}, written by save_all()."""

    def __init__(self):
        self.pages = []
        for config_type in idleConf.config_types:
            self[config_type] = {}
            self.pages.append(self[config_type])

    def add_option(self, config_type, section, item, value):
        """Record a change."""
        page = self[config_type]
        page.setdefault(section, {})[item] = str(value)

    @staticmethod
    def save_option(config_type, section, item, value):
        """Store one value in the user configuration; a value equal to the
        default is removed instead.  Return True if anything changed."""
        default = idleConf.defaultCfg[config_type]
        if default.has_option(section, item):
            if default.Get(section, item) == value:
                return idleConf.userCfg[config_type].RemoveOption(section, item)
        return idleConf.userCfg[config_type].SetOption(section, item, value)

    def save_all(self):
        """Write all recorded changes to the user files and clear them.

        Return True if any configuration was changed.  The main, keys and
        highlight files are written even if unchanged.
        """
        idleConf.userCfg['main'].Save()
        changed = False
        for config_type, page in self.items():
            type_changed = False
            for section, items in page.items():
                if section == 'HelpFiles':
                    # the help list is replaced as a whole
                    idleConf.userCfg['main'].remove_section('HelpFiles')
                    type_changed = True
                for item, value in items.items():
                    if self.save_option(config_type, section, item, value):
                        type_changed = True
            if type_changed:
                idleConf.userCfg[config_type].Save()
                changed = True
        for config_type in ('keys', 'highlight'):
            idleConf.userCfg[config_type].Save()
        self.clear()
        return changed

    def delete_section(self, config_type, section):
        """Delete a section (e.g. a custom theme) from the changes and the
        user configuration."""
        changes = self[config_type]
        if section in changes:
            del changes[section]
        user = idleConf.userCfg[config_type]
        user.remove_section(section)
        user.Save()

    def clear(self):
        """Forget all changes (the per-type dicts remain)."""
        for page in self.pages:
            page.clear()


def _dump():  # for debugging
    from zlib import crc32
    line, crc = 0, 0

    def sprint(obj):
        nonlocal line, crc
        txt = str(obj)
        line += 1
        crc = crc32(txt.encode(encoding='utf-8'), crc)
        print(txt)

    def dumpCfg(cfg):
        print('\n', cfg, '\n')
        for key in sorted(cfg):
            sections = cfg[key].sections()
            sprint(key)
            sprint(sections)
            for section in sections:
                options = cfg[key].options(section)
                sprint(section)
                sprint(options)
                for option in options:
                    sprint(option + ' = ' + cfg[key].Get(section, option))

    dumpCfg(idleConf.defaultCfg)
    dumpCfg(idleConf.userCfg)
    print('\nlines = ', line, ', crc = ', crc, sep='')


if __name__ == '__main__':
    from unittest import main
    main('idlelib.idle_test.test_config', verbosity=2, exit=False)
    _dump()
