"""The names from before rooms were called projects, kept working for one release.

# simplify: delete this module, the `__getattr__` that uses it in `__init__.py` and its
# `renamed_keywords` decorators in 0.4; until then every old name warns where it is used.
"""

import functools
import warnings

RENAMED = {
    "Room": ("Room", "Project"),
    "RoomInfo": ("RoomInfo", "ProjectInfo"),
    "rooms": ("rooms()", "projects()"),
}


def attribute(namespace, name):
    """The object an old module attribute now goes by, with a warning naming the new one."""
    if name not in RENAMED:
        raise AttributeError(f"module 'cocat_rs' has no attribute {name!r}")
    old, new = RENAMED[name]
    warnings.warn(
        f"cocat_rs.{old} is now cocat_rs.{new}", DeprecationWarning, stacklevel=3
    )
    return namespace[new.removesuffix("()")]


def renamed_keywords(**old_to_new):
    """Lets a function still take its old keyword names, warning when one is used."""

    def decorate(function):
        @functools.wraps(function)
        def call(*args, **kwargs):
            for old in old_to_new.keys() & kwargs.keys():
                new = old_to_new[old]
                warnings.warn(
                    f"{old}= is now {new}=", DeprecationWarning, stacklevel=2
                )
                kwargs[new] = kwargs.pop(old)
            return function(*args, **kwargs)

        return call

    return decorate
