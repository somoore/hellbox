#!/usr/bin/env python3.11
# Keep X keyboard focus on the game window.
import time
from Xlib import display, X


def walk(win):
    yield win
    try:
        for child in win.query_tree().children:
            yield from walk(child)
    except Exception:
        pass


def main():
    d = display.Display(":1")
    root = d.screen().root
    while True:
        target = None
        for w in walk(root):
            try:
                name = w.get_wm_name()
            except Exception:
                name = None
            if name and "doom" in str(name).lower():
                target = w
        if target is not None:
            try:
                target.set_input_focus(X.RevertToParent, X.CurrentTime)
                d.sync()
            except Exception:
                pass
        time.sleep(4)


if __name__ == "__main__":
    main()
