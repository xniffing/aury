#!/usr/bin/env python3
"""Independent reference implementation of the gcd task, for cross-implementation
agreement (NOT a generation baseline). Invoked as `gcd.py <fn> <arg…>`; prints
the result formatted the way Aury's `show_value` renders an i64 (a bare integer).
"""
import sys


def gcd(a, b):
    a, b = abs(a), abs(b)
    while b:
        a, b = b, a % b
    return a


FNS = {"gcd": gcd}


def main():
    fn = sys.argv[1]
    args = [int(x) for x in sys.argv[2:]]
    print(FNS[fn](*args))


if __name__ == "__main__":
    main()
