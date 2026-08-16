# Import shapes rr extracts from Python. One construct per grouping in the
# issue's truth table; assertions live in tags.rs unit tests and the golden.
import os
import os.path
import os.path as p
import a, b

from x import y
from x import y as z
from . import x
from ..pkg import y
from x import *
from __future__ import annotations