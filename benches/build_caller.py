#!/usr/bin/env python3
"""Compile the measurement checkout's transfer caller against a different engine."""
import os
from pathlib import Path
import sys


def compiler_args(args, checkout, caller):
    if any(arg == "--print" or arg.startswith("--print=") for arg in args):
        return args
    if "--crate-name" not in args or args[args.index("--crate-name") + 1] != "transfer":
        return args
    declared = checkout / "benches/transfer.rs"
    inputs = [i for i, arg in enumerate(args) if arg.endswith("transfer.rs") and (checkout / arg).resolve() == declared]
    if len(inputs) != 1:
        raise ValueError("expected the declared transfer benchmark source")
    args = args.copy()
    args[inputs[0]] = str(caller)
    return args


if __name__ == "__main__":
    argv = compiler_args(sys.argv[2:], Path.cwd(), Path(__file__).resolve().with_name("transfer.rs"))
    compiler = sys.argv[1]
    inner = os.environ.get("ORBITE_BENCH_INNER_WRAPPER")
    command = [inner, compiler, *argv] if inner else [compiler, *argv]
    os.execvp(command[0], command)
