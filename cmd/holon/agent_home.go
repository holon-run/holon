package main

import (
	"fmt"
	"os"
	"path/filepath"

	"github.com/holon-run/holon/pkg/agenthome"
)

func resolveAgentHome(command, agentID, agentHome string, allowEphemeral bool) (agenthome.Resolution, error) {
	res, err := agenthome.Resolve(agenthome.ResolveOptions{
		Command:          command,
		AgentID:          agentID,
		AgentHome:        agentHome,
		EphemeralAllowed: allowEphemeral,
	})
	if err != nil {
		return agenthome.Resolution{}, err
	}
	if err := agenthome.EnsureLayout(res.AgentHome); err != nil {
		return agenthome.Resolution{}, err
	}
	return res, nil
}

func stateDirForAgentHome(agentHome string) string {
	return filepath.Join(agentHome, "state")
}

func workspaceDirForAgentHome(agentHome string) string {
	return filepath.Join(agentHome, "workspace")
}

func cleanupEphemeralAgentHome(res agenthome.Resolution, cleanupMode string) {
	if !res.Ephemeral {
		return
	}
	if cleanupMode == "none" {
		fmt.Printf("Preserving temporary agent home (cleanup=none): %s\n", res.AgentHome)
		return
	}
	fmt.Printf("Cleaning up temporary agent home: %s\n", res.AgentHome)
	_ = os.RemoveAll(res.AgentHome)
}
