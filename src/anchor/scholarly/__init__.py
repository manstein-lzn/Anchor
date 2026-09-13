"""The literature tools as a command line, so an agent whose only tool is bash can use them.

Nothing here knows about models, graphs or sandboxes. It reads arguments, calls research_tools and
writes the answer to stdout as JSON. Errors go to stderr with a non-zero exit, because that is what
a shell-using agent can act on.
"""
