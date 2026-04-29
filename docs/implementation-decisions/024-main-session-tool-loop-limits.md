# Main Session Tool Loop Limits

Decision:

- do not enforce a default tool-round cap for the main agent
- keep loop limits as optional per-flow controls rather than a global default

Reason:

- coding tasks often need more than a handful of model/tool rounds
- dead-loop prevention is better handled with specific controls
