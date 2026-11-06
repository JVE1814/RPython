README.txt: an overview of the idlelib package

IDLE is the integrated development environment that comes with Python:
a shell window with an interactive interpreter, editor windows with
syntax coloring and smart indentation, a debugger, browsers and search
dialogs.  It is written in Python and uses tkinter for its windows.

This file describes the files of the idlelib directory.  The user
documentation is in help.html (Help > IDLE Help).


STARTING IDLE

  python -m idlelib          start IDLE (the shell window)
  python -m idlelib -h       list the command line options
  idle.py, idle.pyw          start IDLE from a script or an icon

User code runs in a separate process ("the subprocess"), which talks
to the IDLE process through rpc.py.  With -n, user code runs in the
IDLE process itself.


MODULES

Startup and the shell
  __main__.py      python -m idlelib
  idle.py          start IDLE from a file
  pyshell.py       the shell window, startup, the interpreter proxy
  run.py           main loop of the user code process
  rpc.py           remote procedure calls between the two processes
  runscript.py     Run > Run Module and Check Module

Editing
  editor.py        the editor window
  filelist.py      the open editor windows
  iomenu.py        open, save, print, encodings
  outwin.py        output windows (for example of Find in Files)
  window.py        the Window menu
  mainmenu.py      the menu definitions
  multicall.py     key and mouse bindings with modifier combinations
  percolator.py    stack of filters on a Text widget
  delegator.py     base class of the filters
  redirector.py    redirection of Text widget commands
  undo.py          undo and redo
  colorizer.py     syntax coloring
  format.py        format paragraph, indent, comment, tabs, strip spaces
  autocomplete.py  completion of names and paths
  autocomplete_w.py  the completion list
  autoexpand.py    expansion of words found in the text
  calltip.py       call tips
  calltip_w.py     the call tip window
  codecontext.py   the context of the code at the top of the window
  parenmatch.py    highlighting of matching brackets
  history.py       the input history of the shell
  sidebar.py       line numbers and shell prompts
  squeezer.py      folding of long output in the shell
  zzdummy.py       an example extension
  zoomheight.py    toggle maximal window height

Parsing
  pyparse.py       light-weight parsing of Python code for indentation
  hyperparser.py   brackets and expressions around an index

Search
  searchengine.py  the search options and the search itself
  searchbase.py    base class of the search dialogs
  search.py        Find
  replace.py       Replace
  grep.py          Find in Files

Browsers and the debugger
  browser.py       module browser
  pathbrowser.py   path browser
  tree.py          tree widget used by the browsers
  stackviewer.py   stack viewer
  debugger.py      the debugger
  debugger_r.py    the debugger with the remote process
  debugobj.py      tree items of objects
  debugobj_r.py    remote tree items

Dialogs
  configdialog.py  Options > Configure IDLE
  config_key.py    choosing new key bindings
  config.py        configuration files and the user settings
  dynoption.py     an OptionMenu whose items can change
  help.py          the help viewer
  help_about.py    Help > About IDLE
  query.py         small dialogs asking for one value
  textview.py      read-only text windows
  tooltip.py       tooltips
  scrolledlist.py  a list with a scrollbar
  statusbar.py     the status bar

Platforms
  macosx.py        adjustments for macOS
  util.py          small helpers shared by several modules


CONFIGURATION FILES

  config-main.def        general options
  config-highlight.def   the built-in color themes
  config-keys.def        the built-in key sets
  config-extensions.def  the extensions and their options

User changes are saved in the .idlerc directory of the home directory.


TESTS

The idle_test directory contains the unit tests (python -m test
test_idle) and the human tests of htest.py.
