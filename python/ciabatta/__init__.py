"""Ask the person running a ciabatta workflow a question, from Python.

The Python side of ``ciabatta input``: a step written in Python asks its
question with a function call instead of shelling out::

    import ciabatta

    region = ciabatta.input("Which region?", default="us-east-1", env="REGION")
    if ciabatta.confirm("Deploy to production?", default=False):
        ...
    ciabatta.pause("Plug the board in, then continue")

Inside a run the question goes to the run's input broker — the same one
``ciabatta input`` talks to — so it appears wherever the run is being
watched: under the run in a terminal, or as a dialog in the web app. Outside
a run it is asked on this terminal, through the ``ciabatta`` binary when one is
on ``PATH`` and with Python's own ``input()`` when not.

Standard library only, and Python 3.9 or newer.
"""

import builtins
import getpass
import json
import os
import shutil
import socket
import subprocess
import sys
from typing import Optional, Union

__all__ = ["input", "confirm", "pause", "InputError", "__version__"]

__version__ = "0.0.0"

# Where a step finds its run's broker, and the secret a request must carry.
# The run sets both for every step it starts.
ADDR_VAR = "CIABATTA_INPUT_ADDR"
TOKEN_VAR = "CIABATTA_INPUT_TOKEN"
STEP_VAR = "CIABATTA_STEP"

_YES = {"y", "yes", "true", "1", "on"}
_NO = {"n", "no", "false", "0", "off"}


class InputError(RuntimeError):
    """The question couldn't be answered: it timed out with no default,
    nobody could be asked, or the run stopped listening."""


def _yes_no(value: str) -> Optional[str]:
    """``y`` or ``n`` for anything that reads as yes or no, else ``None``."""
    text = value.strip().lower()
    if text in _YES:
        return "y"
    if text in _NO:
        return "n"
    return None


def _timeout(value: Union[None, int, float, str]) -> Optional[str]:
    """A timeout as ``ciabatta input --timeout`` takes it: seconds, or a
    duration such as ``"5m"``."""
    if value is None:
        return None
    if isinstance(value, (int, float)):
        return f"{int(value)}s"
    return str(value)


def _ask(
    prompt: str,
    details: Optional[str],
    default: Optional[str],
    env: Optional[str],
    secret: bool,
    timeout: Union[None, int, float, str],
    enter: bool,
    yes_no: bool,
) -> str:
    if env:
        value = os.environ.get(env, "")
        if value:
            return value

    # Whatever the script printed before asking belongs before the question.
    # Piped (as every step's output is), Python holds stdout back until a
    # buffer fills — so without this the lead-in to a question turned up after
    # it was answered, unless PYTHONUNBUFFERED was set.
    for stream in (sys.stdout, sys.stderr):
        try:
            stream.flush()
        except (AttributeError, OSError, ValueError):
            pass

    addr = os.environ.get(ADDR_VAR)
    token = os.environ.get(TOKEN_VAR)
    if addr and token:
        return _ask_broker(
            addr,
            {
                "token": token,
                "step": os.environ.get(STEP_VAR),
                "prompt": prompt,
                "details": details,
                "default": default,
                "secret": secret,
                "env": env,
                "timeout": _timeout(timeout),
                "enter": enter,
                "yes_no": yes_no,
            },
        )

    binary = shutil.which("ciabatta")
    if binary:
        return _ask_binary(binary, prompt, details, default, env, secret, timeout, enter, yes_no)
    return _ask_here(prompt, details, default, secret, enter, yes_no)


def _ask_broker(addr: str, request: dict) -> str:
    """One request line to the broker, one reply line back. The connection
    stays open while the question waits, which is how the broker knows the
    asker is still there."""
    host, _, port = addr.rpartition(":")
    try:
        with socket.create_connection((host.strip("[]"), int(port))) as conn:
            conn.sendall((json.dumps(request) + "\n").encode("utf-8"))
            reply = b""
            while not reply.endswith(b"\n"):
                chunk = conn.recv(65536)
                if not chunk:
                    break
                reply += chunk
    except OSError as error:
        raise InputError(f"couldn't reach this run's input broker at {addr}: {error}") from error
    if not reply.strip():
        raise InputError("the run ended before answering")
    answer = json.loads(reply.decode("utf-8"))
    if "value" in answer:
        return answer["value"]
    raise InputError(answer.get("error", "no answer"))


def _ask_binary(binary, prompt, details, default, env, secret, timeout, enter, yes_no) -> str:
    """Outside a run, with ciabatta installed: let it draw the prompt, so it
    looks and behaves exactly like ``ciabatta input`` does.

    It draws on stderr, so stderr is this process's own: captured, the prompt
    went into a pipe and the question waited, unseen, for an answer. Only the
    answer, on stdout, is captured.
    """
    if yes_no and not _knows_flag(binary, "--yes-no"):
        # An older ciabatta (--yes-no arrived in 0.7.1): ask here instead.
        return _ask_here(prompt, details, default, secret, enter, yes_no)
    command = [binary, "input", prompt]
    if details is not None:
        command += ["--details", details]
    if default is not None:
        command += ["--default", default]
    if env:
        command += ["--env", env]
    if secret:
        command.append("--secret")
    if timeout is not None:
        command += ["--timeout", _timeout(timeout)]
    if enter:
        command.append("--enter")
    if yes_no:
        command.append("--yes-no")
    done = subprocess.run(command, stdout=subprocess.PIPE, text=True)
    if done.returncode != 0:
        raise InputError(f"'{prompt}' wasn't answered")
    return done.stdout.rstrip("\r\n")


def _knows_flag(binary: str, flag: str) -> bool:
    """Whether this ciabatta's ``input`` takes ``flag``."""
    try:
        done = subprocess.run(
            [binary, "input", "--help"],
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
        )
    except OSError:
        return False
    return flag in done.stdout


def _ask_here(prompt, details, default, secret, enter, yes_no) -> str:
    """No run and no ciabatta: Python's own prompt."""
    if details:
        print(details, file=sys.stderr)
    hint = ""
    if yes_no:
        hint = {"y": " [Y/n]", "n": " [y/N]"}.get(default or "", " [y/n]")
    elif default and not secret and not enter:
        hint = f" [{default}]"
    while True:
        try:
            text = (getpass.getpass if secret else builtins.input)(f"{prompt}{hint} ")
        except EOFError as error:
            if default is not None:
                return default
            raise InputError(f"'{prompt}' needs an answer and stdin had none") from error
        if not text and default is not None:
            return default
        if not yes_no or _yes_no(text) is not None:
            return text


def input(
    prompt: str = "Enter a value",
    *,
    details: Optional[str] = None,
    default: Optional[str] = None,
    env: Optional[str] = None,
    secret: bool = False,
    timeout: Union[None, int, float, str] = None,
) -> str:
    """Ask for a value and return it.

    ``default`` is the answer to an empty reply, a timeout, or a run nobody
    can be asked in. ``env`` names a variable that answers without asking
    when it is set. ``secret`` masks the answer as it is typed and in the
    run's logs. ``timeout`` (seconds, or ``"5m"``) overrides the workspace's
    ``input_timeout`` for this one question.

    Raises :class:`InputError` when there is no answer and no default.
    """
    return _ask(prompt, details, default, env, secret, timeout, enter=False, yes_no=False)


def confirm(
    prompt: str,
    *,
    details: Optional[str] = None,
    default: Optional[bool] = None,
    env: Optional[str] = None,
    timeout: Union[None, int, float, str] = None,
) -> bool:
    """Ask a yes-or-no question: Yes and No buttons in the web app, ``[y/n]``
    in a terminal. ``True`` for yes.

    The answer travels as ``y`` or ``n``, as ``ciabatta input --yes-no``
    prints it; ``env`` may hold ``y``/``n``, ``yes``/``no`` or
    ``true``/``false``.
    """
    fallback = None if default is None else ("y" if default else "n")
    answer = _ask(prompt, details, fallback, env, False, timeout, enter=False, yes_no=True)
    normalized = _yes_no(answer)
    if normalized is None:
        raise InputError(f"'{answer}' isn't yes or no")
    return normalized == "y"


def pause(
    prompt: str = "Press Enter to continue",
    *,
    details: Optional[str] = None,
    timeout: Union[None, int, float, str] = None,
) -> None:
    """Wait for a go-ahead: Enter in a terminal, Continue in the web app. A
    pause that times out, or that nobody can see, carries on."""
    _ask(prompt, details, "", None, False, timeout, enter=True, yes_no=False)
