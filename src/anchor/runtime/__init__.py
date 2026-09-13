"""Runtime pieces this build keeps: the literature tools, the sandbox, and secrets.

Deliberately empty of imports. It used to re-export nineteen modules, which made every one of them
unimportable without the other eighteen — including from an environment that has the one thing a
caller actually wanted.
"""
