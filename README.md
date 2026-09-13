# Anchor

Define an agent graph, run it, and each node works in its own directory inside a sandbox. That is
the whole thing.

```bash
anchor-graph examples/graphs/academic-simple.json --objective "写一篇综述" --work /tmp/run
```

A graph is a JSON file. A node is an agent with a directory. An edge says which nodes' work a node
starts from. A run seeds each node's directory with its inputs' directories, lets it work, and hands
the result to the next one.

```json
{
  "objective": "what this graph is for, as the default task text",
  "agents": {
    "gatherer": {
      "model": "models.academic",
      "network": true,
      "instructions": "You gather academic evidence…"
    }
  },
  "nodes": [{"id": "gather", "agent": "gatherer"}],
  "edges": [{"from": "plan", "to": "gather"}]
}
```

## What is ours and what is not

**Ours:** the graph, the directories, the sandbox, and the literature tools.

**Not ours:** the agent loop. That is [mini-swe-agent](https://github.com/SWE-agent/mini-swe-agent)
(MIT), because a loop that cannot be stopped by talking is a property of the loop, and writing one
that has that property is harder than it looks. Theirs does: a response without a command is a
format error and is retried, and a run ends only when a command asks for submission. In a CLI,
someone follows up when an agent stops halfway. In a node, nobody does — so the node must not be
able to stop that way.

## The sandbox

Every command a node runs goes through `bwrap`:

- the node's own directory is writable, everything else is read-only
- the network is off unless the node's agent says `"network": true`
- the node's PATH carries `anchor-scholarly` and nothing else of ours

An allowlist of commands was considered and rejected: mini runs commands through a shell, so the
shell is the entry point and the sandbox is the boundary. An allowlist in front of a shell is a
second, weaker boundary that the shell steps around.

## The literature tools

```bash
anchor-scholarly search   --query "learned cost models" [--source crossref|arxiv] [--limit 8]
anchor-scholarly read     --url "https://arxiv.org/pdf/2401.00001"
anchor-scholarly read-many --urls "u1,u2,u3"
anchor-scholarly citations --identifier 2401.00001 [--direction cited_by|cites]
```

JSON on stdout; a failure exits non-zero and says why on stderr, which is the only failure signal a
shell-using agent can act on. Sources are rate limited and sometimes refuse, and that is information
rather than a dead end — an agent that hits a 429 can try another source, and the trace will show it
did.

They are a command on purpose. It keeps keys, rate limiting and the SSRF check inside a process we
control rather than inside a sandbox, and it means the same tools work from a shell, from any agent
body, and from ours.

## Where a run leaves things

```
<work>/<node>/
  …                 whatever the node produced
  trace.jsonl       the conversation: every message, in order
```

`trace.jsonl` is the debugging surface. Without it, what an agent did has to be inferred by
re-running it; with it, "it searched 47 times and adapted around a source that kept refusing" is a
thing you read.

## What is not here

No runs database, no leases, no recovery, no graph versions, no approval gates, no evidence
ledger, no context engine, no web console, no services. Those existed and were removed: they were
built before anything ran end to end, and what they mostly did was make runs stop without saying so.

`DECISIONS.md` keeps the record of that, including the parts that were mistakes.
