package tools

// RequiredCommands defines the built-in runtime tools contract.
// The list is internal to Holon and intentionally has no user override.
var RequiredCommands = []string{
	"bash",
	"git",
	"curl",
	"jq",
	"rg",
	"find",
	"sed",
	"awk",
	"xargs",
	"tar",
	"gzip",
	"unzip",
	"python3",
	"node",
	"npm",
	"gh",
	"yq",
	"fd",
	"make",
	"patch",
}

// RequiredCommandsList returns a copy so callers cannot mutate the contract.
func RequiredCommandsList() []string {
	out := make([]string, len(RequiredCommands))
	copy(out, RequiredCommands)
	return out
}
