Enqueue a follow-up message into the current agent.

The follow-up is agent-generated, not a new operator instruction. A follow-up from an operator-instruction turn uses runtime-instruction authority; all other authority classes are preserved. Enqueue never grants authority, and provenance references do not carry authorization.
