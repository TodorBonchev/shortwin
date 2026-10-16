"""Puts `docs/design/sim/attacks` on the import path of the drivers in this directory (run them
from anywhere: `python3 tests/litesvm/sim/<driver>.py`)."""
import os
import sys

ATTACKS = os.path.normpath(os.path.join(os.path.dirname(os.path.abspath(__file__)),
                                        "..", "..", "..", "docs", "design", "sim", "attacks"))
sys.path.insert(0, ATTACKS)
