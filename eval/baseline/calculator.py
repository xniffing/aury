#!/usr/bin/env python3
"""Independent reference implementation of the calculator task's checked
functions, for cross-implementation agreement (NOT a generation baseline).
Invoked as `calculator.py <fn> <arg…>`; prints the result the way Aury renders
an i64 (a bare integer).
"""
import sys


def add(a, b):
    return a + b


def factorial(n):
    acc = 1
    for i in range(2, n + 1):
        acc *= i
    return acc


def gcd(a, b):
    a, b = abs(a), abs(b)
    while b:
        a, b = b, a % b
    return a


FNS = {"add": add, "factorial": factorial, "gcd": gcd}


def main():
    fn = sys.argv[1]
    args = [int(x) for x in sys.argv[2:]]
    print(FNS[fn](*args))


if __name__ == "__main__":
    main()
