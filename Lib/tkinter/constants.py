# Symbolic names for the string values Tk accepts as option values.
# `from tkinter.constants import *` (or `from tkinter import *`) brings them in.

# Booleans
NO = FALSE = OFF = 0
YES = TRUE = ON = 1

# -anchor and -sticky
N = 'n'
S = 's'
W = 'w'
E = 'e'
NW = 'nw'
SW = 'sw'
NE = 'ne'
SE = 'se'
NS = 'ns'
EW = 'ew'
NSEW = 'nsew'
CENTER = 'center'

# -fill
NONE = 'none'
X = 'x'
Y = 'y'
BOTH = 'both'

# -side
LEFT = 'left'
TOP = 'top'
RIGHT = 'right'
BOTTOM = 'bottom'

# -relief
RAISED = 'raised'
SUNKEN = 'sunken'
FLAT = 'flat'
RIDGE = 'ridge'
GROOVE = 'groove'
SOLID = 'solid'

# -orient
HORIZONTAL = 'horizontal'
VERTICAL = 'vertical'

# -tabs
NUMERIC = 'numeric'

# -wrap
CHAR = 'char'
WORD = 'word'

# -align
BASELINE = 'baseline'

# -bordermode
INSIDE = 'inside'
OUTSIDE = 'outside'

# Special tags, marks and insert positions
SEL = 'sel'
SEL_FIRST = 'sel.first'
SEL_LAST = 'sel.last'
END = 'end'
INSERT = 'insert'
CURRENT = 'current'
ANCHOR = 'anchor'
ALL = 'all'

# Widget states
NORMAL = 'normal'
DISABLED = 'disabled'
ACTIVE = 'active'
HIDDEN = 'hidden'

# Menu item types
CASCADE = 'cascade'
CHECKBUTTON = 'checkbutton'
COMMAND = 'command'
RADIOBUTTON = 'radiobutton'
SEPARATOR = 'separator'

# Listbox selection modes
SINGLE = 'single'
BROWSE = 'browse'
MULTIPLE = 'multiple'
EXTENDED = 'extended'

# Listbox -activestyle
DOTBOX = 'dotbox'
# UNDERLINE is also an -activestyle value
UNDERLINE = 'underline'

# Canvas arc styles
PIESLICE = 'pieslice'
CHORD = 'chord'
ARC = 'arc'
# Canvas line arrows, cap and join styles
FIRST = 'first'
LAST = 'last'
BUTT = 'butt'
PROJECTING = 'projecting'
ROUND = 'round'
BEVEL = 'bevel'
MITER = 'miter'

# Scrollbar / view commands
MOVETO = 'moveto'
SCROLL = 'scroll'
UNITS = 'units'
PAGES = 'pages'
