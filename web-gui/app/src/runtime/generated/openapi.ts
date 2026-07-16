// Generated from docs/website/reference/openapi.json by web-gui/openapi-tools.
// Do not edit by hand. Run `make transport-types` from the repository root.
export interface paths {
    "/api/": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Root discovery
         * @description Return the default agent id.
         */
        get: operations["root"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/agents/list": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * List agents
         * @description Return lightweight public agent entries.
         */
        get: operations["listAgents"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/agents/{agent_id}/briefs": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Recent briefs
         * @description Return recent user-facing delivery briefs. Query parameter: limit.
         */
        get: operations["agentBriefs"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/agents/{agent_id}/briefs/{brief_id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Brief detail
         * @description Return a persisted user-facing delivery brief by id.
         */
        get: operations["agentBrief"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/agents/{agent_id}/enqueue": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Enqueue agent message
         * @description Enqueue a public channel/webhook message for the named agent.
         */
        post: operations["enqueueAgent"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/agents/{agent_id}/events": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Agent event page
         * @description Return a bounded page of runtime event envelopes. Query parameters: before_seq, after_seq, limit, order, max_level. Event payloads are included in full; max_level filters event inclusion only. Breaking change: the projection query parameter and StreamEventEnvelope.projection field have been removed.
         */
        get: operations["agentEvents"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/agents/{agent_id}/events/stream": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Agent event stream
         * @description Return Server-Sent Events carrying raw StreamEventEnvelope JSON data. Query parameters: after_seq, limit. SSE id is event_seq; SSE event is the audit event kind; missing replay cursors return cursor_not_found before the stream opens. If the receiver lags, the server closes the stream so clients can backfill after the last contiguous SSE id before reconnecting. Breaking change: the projection query parameter and StreamEventEnvelope.projection field have been removed.
         */
        get: operations["agentEventsStream"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/agents/{agent_id}/messages/{message_id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Message detail
         * @description Return a persisted message envelope by id for the selected agent.
         */
        get: operations["agentMessage"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/agents/{agent_id}/messages:batchGet": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Batch get messages
         * @description Return persisted message envelopes for the selected agent. Missing or cross-agent ids are reported in missing_message_ids.
         */
        post: operations["agentMessagesBatchGet"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/agents/{agent_id}/skills": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * List agent skills
         * @description Return skills enabled/effective for an agent.
         */
        get: operations["agentSkills"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/agents/{agent_id}/state": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Agent state snapshot
         * @description Return the lightweight bootstrap snapshot for an agent. Heavy task, work-item, operator notification, and execution details are available through dedicated routes and events.
         */
        get: operations["agentState"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/agents/{agent_id}/status": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Agent status
         * @description Return the public AgentSummary read model.
         */
        get: operations["agentStatus"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/agents/{agent_id}/tasks": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * List active tasks
         * @description Return active task records. Query parameter: limit.
         */
        get: operations["agentTasks"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/agents/{agent_id}/tasks/{task_id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Task status
         * @description Return a task lifecycle snapshot by id.
         */
        get: operations["agentTaskStatus"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/agents/{agent_id}/tasks/{task_id}/output": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Task output
         * @description Return a task output snapshot. Query parameters: block, timeout_ms.
         */
        get: operations["agentTaskOutput"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/agents/{agent_id}/timers": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * List timers
         * @description Return recent timer records. Query parameter: limit.
         */
        get: operations["agentTimers"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/agents/{agent_id}/timers/{timer_id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Timer detail
         * @description Return a timer record by id.
         */
        get: operations["agentTimer"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/agents/{agent_id}/tool-executions/{tool_execution_id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Tool execution detail
         * @description Return a persisted tool execution record by id.
         */
        get: operations["agentToolExecution"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/agents/{agent_id}/tool-executions/{tool_execution_id}/artifacts/{artifact_index}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Tool execution artifact
         * @description Return UTF-8 content for an artifact referenced by the selected tool execution. Artifact paths are resolved server-side and confined to the agent runtime data directory.
         */
        get: operations["agentToolExecutionArtifact"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/agents/{agent_id}/transcript": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Recent transcript
         * @description Return recent transcript entries. Query parameter: limit.
         */
        get: operations["agentTranscript"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/agents/{agent_id}/transcript/{entry_id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Transcript entry detail
         * @description Return a persisted transcript entry by id for the selected agent.
         */
        get: operations["agentTranscriptEntry"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/agents/{agent_id}/transcript:batchGet": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Batch get transcript entries
         * @description Return persisted transcript entries for the selected agent. Missing or cross-agent ids are reported in missing_entry_ids.
         */
        post: operations["agentTranscriptBatchGet"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/agents/{agent_id}/work-items": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * List work items
         * @description Return latest work item records for the agent. Query parameter: limit.
         */
        get: operations["agentWorkItems"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/agents/{agent_id}/work-items/{work_item_id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Work item detail
         * @description Return a work item record by id.
         */
        get: operations["agentWorkItem"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/agents/{agent_id}/worktree-summary": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Worktree summary
         * @description Return managed worktree summary for an agent.
         */
        get: operations["agentWorktreeSummary"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/auth/codex/device/start": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Start Codex device login
         * @description Request an OpenAI Codex device code and start a background job that persists the OAuth credential profile after user authorization.
         */
        post: operations["startCodexDeviceLogin"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/auth/{provider}/device/start": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Start OAuth device login
         * @description Request a provider OAuth device code and start a background job that persists the OAuth credential profile after user authorization. Supported providers include openai-codex and xai.
         */
        post: operations["startOAuthDeviceLogin"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/briefs": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Default agent briefs alias
         * @description Compatibility alias for the default agent briefs route. Query parameter: limit.
         */
        get: operations["defaultBriefs"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/callbacks/enqueue/{callback_token}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Callback enqueue ingress
         * @description Capability-token callback ingress for enqueue delivery. The token is a secret path segment and examples intentionally use a placeholder.
         */
        post: operations["callbackEnqueue"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/callbacks/wake/{callback_token}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Callback wake ingress
         * @description Capability-token callback ingress for wake delivery. The token is a secret path segment and examples intentionally use a placeholder.
         */
        post: operations["callbackWake"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/control/agents/{agent_id}/control": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Control agent lifecycle
         * @description Submit a lifecycle control action.
         */
        post: operations["controlAgent"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/control/agents/{agent_id}/create": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Create named agent
         * @description Create a public named agent, optionally from a template.
         */
        post: operations["createAgent"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/control/agents/{agent_id}/current-run/abort": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Abort current run
         * @description Request abort for the current agent run.
         */
        post: operations["abortCurrentRun"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/control/agents/{agent_id}/debug-prompt": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Debug prompt
         * @description Render a diagnostic prompt preview.
         */
        post: operations["debugPrompt"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/control/agents/{agent_id}/model": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Set agent model override
         * @description Set an agent model override and optional reasoning effort.
         */
        post: operations["setAgentModel"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/control/agents/{agent_id}/model/clear": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Clear agent model override
         * @description Clear an agent model override.
         */
        post: operations["clearAgentModel"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/control/agents/{agent_id}/operator-bindings": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Create operator binding
         * @description Create or update a remote operator transport binding.
         */
        post: operations["createOperatorTransportBinding"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/control/agents/{agent_id}/operator-ingress": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Operator ingress
         * @description Deliver an authenticated remote operator prompt.
         */
        post: operations["operatorIngress"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/control/agents/{agent_id}/prompt": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Submit operator prompt
         * @description Submit a trusted operator prompt through the control plane.
         */
        post: operations["controlPrompt"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/control/agents/{agent_id}/reset-callback": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Reset external trigger callback
         * @description Revoke the current external trigger and provision a fresh one with a new token.
         */
        post: operations["resetCallback"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/control/agents/{agent_id}/skills/disable": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Disable agent skill
         * @description Disable a skill for an agent.
         */
        post: operations["disableSkill"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/control/agents/{agent_id}/skills/enable": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Enable agent skill
         * @description Enable a locally known skill for an agent.
         */
        post: operations["enableSkill"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/control/agents/{agent_id}/skills/install": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Install skill compatibility alias
         * @description Compatibility alias for older agent skill install behavior.
         */
        post: operations["installSkill"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/control/agents/{agent_id}/skills/uninstall": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Uninstall skill compatibility alias
         * @description Compatibility alias for disabling an agent skill.
         */
        post: operations["uninstallSkill"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/control/agents/{agent_id}/tasks": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Create command task
         * @description Schedule a command task for an agent.
         */
        post: operations["createCommandTask"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/control/agents/{agent_id}/tasks/{task_id}/input": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Task input
         * @description Deliver text input to a managed task.
         */
        post: operations["taskInput"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/control/agents/{agent_id}/tasks/{task_id}/stop": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Task stop
         * @description Request cancellation for a managed task.
         */
        post: operations["taskStop"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/control/agents/{agent_id}/timers": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Create timer
         * @description Schedule a timer for an agent.
         */
        post: operations["createTimer"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/control/agents/{agent_id}/timers/{timer_id}/cancel": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Cancel timer
         * @description Cancel an active timer. Cancellation is idempotent for already-cancelled timers; completed or missing timers return a shared error envelope.
         */
        post: operations["cancelTimer"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/control/agents/{agent_id}/wake": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Wake agent
         * @description Submit a trusted wake hint.
         */
        post: operations["controlWake"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/control/agents/{agent_id}/work-items": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Create work item
         * @description Create or enqueue a public work item objective.
         */
        post: operations["createWorkItem"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/control/agents/{agent_id}/work-items/{work_item_id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        /**
         * Update work item
         * @description Mutate work item objective, plan status, todo list, or blocker fields.
         */
        patch: operations["updateWorkItem"];
        trace?: never;
    };
    "/api/control/agents/{agent_id}/work-items/{work_item_id}/complete": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Complete work item
         * @description Mark an open work item completed.
         */
        post: operations["completeWorkItem"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/control/agents/{agent_id}/work-items/{work_item_id}/pick": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Pick work item
         * @description Make an existing open work item the current focus for the agent.
         */
        post: operations["pickWorkItem"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/control/agents/{agent_id}/workspace/attach": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Attach workspace
         * @description Attach a workspace path to an agent.
         */
        post: operations["attachWorkspace"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/control/agents/{agent_id}/workspace/detach": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Detach workspace
         * @description Detach a workspace binding by workspace id.
         */
        post: operations["detachWorkspace"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/control/agents/{agent_id}/workspace/exit": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Exit workspace
         * @description Return an agent to its default AgentHome workspace.
         */
        post: operations["exitWorkspace"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/control/runtime/config": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Runtime config
         * @description Return the daemon effective runtime configuration surface.
         */
        get: operations["runtimeConfig"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        /**
         * Update runtime config
         * @description Persist runtime-mutable config updates and classify their effect as restart/reload-required or rejected.
         */
        patch: operations["runtimeConfigUpdate"];
        trace?: never;
    };
    "/api/control/runtime/config/migrate-model-routes": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Migrate model config routes
         * @description Inspect legacy model route references or persist a complete canonical migration across config.json and agent state.
         */
        post: operations["migrateModelConfigRoutes"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/control/runtime/credentials": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Runtime credential profiles
         * @description List credential profiles stored in the runtime credential store.
         */
        get: operations["runtimeCredentials"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/control/runtime/credentials/{profile}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        /**
         * Set runtime credential
         * @description Set an API key credential profile in the runtime credential store.
         */
        put: operations["setRuntimeCredential"];
        post?: never;
        /**
         * Delete runtime credential
         * @description Remove a credential profile from the runtime credential store.
         */
        delete: operations["deleteRuntimeCredential"];
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/control/runtime/performance": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Runtime performance diagnostics
         * @description Return bounded in-process performance diagnostics for HTTP, projections, DB, and scheduler activity.
         */
        get: operations["runtimePerformance"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/control/runtime/readiness": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Runtime readiness
         * @description Return daemon readiness metadata.
         */
        get: operations["runtimeReadiness"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/control/runtime/shutdown": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Runtime shutdown
         * @description Request graceful runtime shutdown.
         */
        post: operations["runtimeShutdown"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/control/runtime/status": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Runtime status
         * @description Return daemon status and runtime activity metadata.
         */
        get: operations["runtimeStatus"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/control/templates/install": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Install template
         * @description Install a template package from a GitHub tree URL into the user global library.
         */
        post: operations["installTemplate"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/control/templates/remove": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Remove template
         * @description Remove a template from the user global library.
         */
        post: operations["removeTemplate"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/enqueue": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Enqueue default agent message
         * @description Enqueue a public channel/webhook message for the default agent.
         */
        post: operations["enqueueDefault"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/events/stream": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Global event stream
         * @description Return Server-Sent Events carrying raw StreamEventEnvelope JSON data for all public agents. This live stream uses the in-memory event watcher and does not provide historical replay or a global cursor. If the receiver lags, the server closes the stream; clients must backfill each agent from its last contiguous event_seq before reconnecting.
         */
        get: operations["eventsStream"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/handshake": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Protocol handshake
         * @description Return auth mode, protocol version, capabilities, and runtime hints.
         */
        get: operations["handshake"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/jobs": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Create job
         * @description Create an asynchronous job. Currently supports kind=skill.install for Global Skill Library installation.
         */
        post: operations["createJob"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/jobs/{job_id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Job status
         * @description Return a generic asynchronous job snapshot by id.
         */
        get: operations["jobStatus"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/memory/get": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Fetch runtime memory source
         * @description Fetch exact bounded memory content by source_ref, matching the agent MemoryGet tool contract.
         */
        post: operations["runtimeMemoryGet"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/models": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * List available models
         * @description Return model catalog entries and runtime availability.
         */
        get: operations["models"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/search": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Search runtime memory
         * @description Search the same memory v2 index used by the agent MemorySearch tool.
         */
        post: operations["runtimeSearch"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/skills/catalog": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Skills catalog
         * @description Return the global user Skill Library catalog. Query parameter: scope.
         */
        get: operations["skillsCatalog"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/skills/catalog/add": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Add skill to library
         * @description Add or import a skill into the local Skill Library.
         */
        post: operations["addSkillToCatalog"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/skills/catalog/check": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Check skill library
         * @description Check Skill Library and lock-file consistency.
         */
        post: operations["checkSkillCatalog"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/skills/catalog/reconcile": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Reconcile skill library lock
         * @description Reconcile local Skill Library contents with .skill-lock.json, then check consistency. This does not fetch remote updates.
         */
        post: operations["reconcileSkillCatalog"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/skills/catalog/refresh": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Refresh runtime catalog
         * @description Refresh runtime Skill Library catalog by rescanning local skill roots. Does not reconcile with lock file or fetch remote updates.
         */
        post: operations["refreshSkillCatalog"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/skills/catalog/remove": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Remove skill from library
         * @description Remove a skill from the local Skill Library.
         */
        post: operations["removeSkillFromCatalog"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/skills/catalog/update": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Update skill library
         * @description Queue an asynchronous update of supported remote Skill Library entries described by .skill-lock.json. Progress and per-skill results are available through the returned job.
         */
        post: operations["updateSkillCatalog"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/skills/catalog/{skill_id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Skill detail
         * @description Return catalog metadata and SKILL.md content for a Global Skill Library skill.
         */
        get: operations["skillDetail"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/state": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Default agent state alias
         * @description Compatibility alias for the default agent state route.
         */
        get: operations["defaultState"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/status": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Default agent status alias
         * @description Compatibility alias for the default agent status route.
         */
        get: operations["defaultStatus"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/templates/catalog": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Template catalog
         * @description Return the global AgentTemplate catalog (user global library + synced remote sources).
         */
        get: operations["templatesCatalog"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/templates/catalog/check": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Check template
         * @description Validate a local template directory without applying it.
         */
        post: operations["checkTemplate"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/templates/catalog/{catalog_id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Template detail
         * @description Return template detail with full AGENTS.md content, manifest, and skill dependencies.
         */
        get: operations["templateDetail"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/templates/remote-sources/sync": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Sync remote template sources
         * @description Queue a daemon job that synchronizes configured AgentTemplate remote sources.
         */
        post: operations["syncTemplateRemoteSources"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/transcript": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Default agent transcript alias
         * @description Compatibility alias for the default agent transcript route. Query parameter: limit.
         */
        get: operations["defaultTranscript"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/webhooks/generic/{agent_id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Generic webhook
         * @description Convert an arbitrary JSON webhook body into a trusted integration message.
         */
        post: operations["genericWebhook"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/workspaces/{workspace_id}/files": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Browse workspace root
         * @description List directory entries at the workspace root. Query parameters: execution_root_id.
         */
        get: operations["workspaceFilesRoot"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/workspaces/{workspace_id}/files/{path}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Browse workspace files
         * @description List a directory or read a file by path. Supports content negotiation: Accept: application/json returns structured metadata + content, other Accept values return raw body. Query parameters: execution_root_id, download, meta.
         */
        get: operations["workspaceFiles"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/worktree-summary": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Default agent worktree summary alias
         * @description Compatibility alias for the default agent worktree summary route.
         */
        get: operations["defaultWorktreeSummary"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
}
export type webhooks = Record<string, never>;
export interface components {
    schemas: {
        /** @description Baseline request DTO schema. Per-field schemas will be tightened as HTTP envelope and DTO contracts stabilize. */
        AbortCurrentRunRequest: {
            [key: string]: unknown;
        };
        /** AddSkillRequest */
        AddSkillRequest: {
            kind: {
                /** @constant */
                kind: "named";
                /**
                 * @default linked
                 * @enum {string}
                 */
                mode: "linked" | "copied";
                name: string;
            } | {
                /** @constant */
                kind: "local";
                /**
                 * @default linked
                 * @enum {string}
                 */
                mode: "linked" | "copied";
                path: string;
            } | {
                /** @constant */
                kind: "remote";
                /**
                 * @default linked
                 * @enum {string}
                 */
                mode: "linked" | "copied";
                package: string;
                skill?: string | null;
            };
        };
        /** AgentStateSnapshotDto */
        AgentStateSnapshotDto: {
            agent: {
                active_children?: {
                    /** Format: uint */
                    active_task_count: number;
                    current_run_id?: string | null;
                    identity: {
                        agent_id: string;
                        delegated_from_task_id?: string | null;
                        is_default_agent: boolean;
                        /** @enum {string} */
                        kind: "default" | "named" | "child";
                        lineage_parent_agent_id?: string | null;
                        /** @enum {string} */
                        ownership: "parent_supervised" | "self_owned";
                        parent_agent_id?: string | null;
                        /** @enum {string} */
                        profile_preset: "private_child" | "public_named";
                        /** @enum {string} */
                        status: "active" | "archived";
                        /** @enum {string} */
                        visibility: "public" | "private";
                    };
                    observability: {
                        /** @enum {string|null} */
                        blocked_reason?: "managed_task_queued" | "managed_task_running" | "managed_task_cancelling" | "awaiting_managed_task" | null;
                        current_work_item_id?: string | null;
                        last_progress_brief?: string | null;
                        last_result_brief?: string | null;
                        /** @enum {string} */
                        phase: "running" | "blocked" | "waiting" | "terminal";
                        /** @enum {string|null} */
                        waiting_reason?: "awaiting_operator_input" | "awaiting_external_change" | "awaiting_task_result" | "awaiting_timer" | null;
                        work_summary?: string | null;
                    };
                    /** Format: uint */
                    pending: number;
                    /** @enum {string} */
                    status: "booting" | "awake_idle" | "awake_running" | "awaiting_task" | "asleep" | "stopped";
                }[];
                /**
                 * Format: uint
                 * @default 0
                 */
                active_task_count: number;
                agent: {
                    active_workspace_entry?: {
                        /** @enum {string} */
                        access_mode: "shared_read" | "exclusive_write";
                        cwd: string;
                        execution_root: string;
                        execution_root_id: string;
                        /** @enum {string} */
                        projection_kind: "canonical_root" | "git_worktree_root";
                        workspace_anchor: string;
                        workspace_id: string;
                    } | null;
                    /** @default [] */
                    attached_workspaces: string[];
                    current_run_id?: string | null;
                    current_turn_id?: string | null;
                    current_turn_work_item_id?: string | null;
                    current_work_item_id?: string | null;
                    id: string;
                    /** Format: date-time */
                    last_brief_at?: string | null;
                    last_runtime_failure?: {
                        detail_hint?: string | null;
                        failure_artifact?: {
                            /** @enum {string} */
                            category: "transport" | "protocol" | "runtime" | "task" | "unknown";
                            /** Format: int32 */
                            exit_status?: number | null;
                            kind: string;
                            metadata?: {
                                [key: string]: string;
                            };
                            model_ref?: string | null;
                            provider?: string | null;
                            source_chain?: string[];
                            /** Format: uint16 */
                            status?: number | null;
                            summary: string;
                            task_id?: string | null;
                        } | null;
                        /** Format: date-time */
                        occurred_at: string;
                        /** @enum {string} */
                        phase: "startup" | "shutdown" | "runtime_turn";
                        summary: string;
                    } | null;
                    last_turn_terminal?: {
                        checkpoint?: {
                            /** Format: uint64 */
                            checkpoint_anchor_generation: number;
                            /** Format: uint64 */
                            current_anchor_generation: number;
                            request_id: string;
                            /** Format: uint */
                            requested_at_round: number;
                            /** Format: uint */
                            response_round?: number | null;
                            /** Format: uint64 */
                            source_turn_index?: number | null;
                            text: string;
                        } | null;
                        /** Format: date-time */
                        completed_at: string;
                        /** Format: uint64 */
                        duration_ms: number;
                        /** @enum {string} */
                        kind: "completed" | "aborted" | "baseline_over_budget" | "deferred_to_fallback" | "provider_failed_needs_recovery";
                        last_assistant_message?: string | null;
                        reason?: string | null;
                        turn_id?: string;
                        /** Format: uint64 */
                        turn_index: number;
                    } | null;
                    last_wake_reason?: string | null;
                    /** Format: uint */
                    pending: number;
                    /** Format: date-time */
                    sleeping_until?: string | null;
                    /** @enum {string} */
                    status: "booting" | "awake_idle" | "awake_running" | "awaiting_task" | "asleep" | "stopped";
                    /**
                     * Format: uint64
                     * @default 0
                     */
                    turn_index: number;
                };
                closure: {
                    /** @enum {string} */
                    outcome: "completed" | "continuable" | "failed" | "waiting";
                    /** @enum {string} */
                    runtime_posture: "awake" | "sleeping";
                    /** @enum {string|null} */
                    waiting_reason?: "awaiting_operator_input" | "awaiting_external_change" | "awaiting_task_result" | "awaiting_timer" | null;
                };
                identity: {
                    agent_id: string;
                    delegated_from_task_id?: string | null;
                    is_default_agent: boolean;
                    /** @enum {string} */
                    kind: "default" | "named" | "child";
                    lineage_parent_agent_id?: string | null;
                    /** @enum {string} */
                    ownership: "parent_supervised" | "self_owned";
                    parent_agent_id?: string | null;
                    /** @enum {string} */
                    profile_preset: "private_child" | "public_named";
                    /** @enum {string} */
                    status: "active" | "archived";
                    /** @enum {string} */
                    visibility: "public" | "private";
                };
                /**
                 * @default {
                 *       "accepts_external_messages": true
                 *     }
                 */
                lifecycle: {
                    accepts_external_messages: boolean;
                    operator_hint?: string | null;
                };
                model: {
                    active_model?: string | null;
                    effective_fallback_models?: string[];
                    effective_model: string;
                    /** @default false */
                    fallback_active: boolean;
                    override_model?: string | null;
                    override_reasoning_effort?: string | null;
                    requested_model?: string | null;
                    runtime_default_model: string;
                    /** @enum {string} */
                    source: "runtime_default" | "agent_override";
                };
                /**
                 * @default {
                 *       "posture": "unknown",
                 *       "reason": "posture projection unavailable"
                 *     }
                 */
                scheduling_posture: {
                    /** @enum {string} */
                    posture: "unknown" | "archived" | "active_turn" | "has_queued_input" | "has_runnable_work" | "waiting_for_task" | "waiting_for_external" | "waiting_for_operator" | "blocked" | "idle";
                    reason: string;
                    run_id?: string | null;
                    task_id?: string | null;
                    work_item_id?: string | null;
                };
            };
            /** @default [] */
            external_triggers: {
                /** Format: date-time */
                created_at: string;
                /** Format: uint64 */
                delivery_count: number;
                /** @enum {string} */
                delivery_mode: "enqueue_message" | "wake_hint";
                external_trigger_id: string;
                /** Format: date-time */
                last_delivered_at?: string | null;
                /** Format: date-time */
                revoked_at?: string | null;
                /** @enum {string} */
                scope: "agent";
                /** @enum {string} */
                status: "active" | "revoked";
                target_agent_id: string;
            }[];
            session: {
                current_run_id?: string | null;
                last_turn?: {
                    checkpoint?: {
                        /** Format: uint64 */
                        checkpoint_anchor_generation: number;
                        /** Format: uint64 */
                        current_anchor_generation: number;
                        request_id: string;
                        /** Format: uint */
                        requested_at_round: number;
                        /** Format: uint */
                        response_round?: number | null;
                        /** Format: uint64 */
                        source_turn_index?: number | null;
                        text: string;
                    } | null;
                    /** Format: date-time */
                    completed_at: string;
                    /** Format: uint64 */
                    duration_ms: number;
                    /** @enum {string} */
                    kind: "completed" | "aborted" | "baseline_over_budget" | "deferred_to_fallback" | "provider_failed_needs_recovery";
                    last_assistant_message?: string | null;
                    reason?: string | null;
                    turn_id?: string;
                    /** Format: uint64 */
                    turn_index: number;
                } | null;
                /** Format: uint */
                pending_count: number;
            };
            tasks: {
                agent_id: string;
                /** Format: date-time */
                created_at: string;
                id: string;
                /** @enum {string} */
                kind: "command_task" | "child_agent_task" | "sleep_job" | "subagent_task" | "worktree_subagent_task";
                parent_message_id?: string | null;
                /** @enum {string} */
                status: "queued" | "running" | "cancelling" | "completed" | "failed" | "cancelled" | "interrupted";
                summary?: string | null;
                /** Format: date-time */
                updated_at: string;
                work_item_id?: string | null;
            }[];
            /** @default [] */
            timers: {
                agent_id: string;
                /** Format: date-time */
                created_at: string;
                /** Format: uint64 */
                duration_ms: number;
                /**
                 * Format: uint64
                 * @default 0
                 */
                fire_count: number;
                id: string;
                /** Format: uint64 */
                interval_ms?: number | null;
                /**
                 * Format: date-time
                 * @default null
                 */
                last_fired_at: string | null;
                /**
                 * Format: date-time
                 * @default null
                 */
                next_fire_at: string | null;
                repeat: boolean;
                /**
                 * @default active
                 * @enum {string}
                 */
                status: "active" | "completed" | "cancelled";
                summary?: string | null;
            }[];
            /** @default [] */
            work_items: {
                agent_id: string;
                blocked_by?: string | null;
                /** @enum {string} */
                candidate_class: "current_runnable" | "triggered_blocked" | "queued_runnable" | "waiting_for_operator" | "yielded" | "blocked" | "completed_recent";
                /** Format: date-time */
                created_at: string;
                current_todo?: {
                    /** @enum {string} */
                    state: "pending" | "in_progress" | "completed";
                    text: string;
                } | null;
                /** @enum {string} */
                focus: "current" | "queued" | "yielded" | "blocked" | "completed";
                id: string;
                is_current: boolean;
                is_runnable: boolean;
                objective: string;
                /** @enum {string} */
                plan_status: "draft" | "ready" | "needs_input";
                /** @enum {string} */
                readiness: "runnable" | "yielded" | "waiting_for_operator" | "blocked" | "completed";
                /** @enum {string} */
                reason_code: "completed" | "continuation_yielded" | "active_task_wait" | "active_operator_wait" | "active_timer_wait" | "active_external_wait" | "active_system_wait" | "manual_blocker" | "plan_needs_input" | "runnable";
                /** Format: date-time */
                recheck_at?: string | null;
                result_brief_id?: string | null;
                result_summary?: string | null;
                /** Format: uint64 */
                revision: number;
                /** @enum {string} */
                scheduling_state: "runnable" | "yielded_to_work_item" | "waiting_operator" | "waiting_task" | "waiting_external" | "waiting_timer" | "waiting_system" | "blocked" | "completed";
                /** @enum {string} */
                state: "open" | "completed";
                turn_id?: string | null;
                /** Format: date-time */
                updated_at: string;
                workspace_id: string;
            }[];
            /**
             * @default {
             *       "workspaces": []
             *     }
             */
            workspace: {
                /** @default [] */
                workspaces: {
                    /** @enum {string|null} */
                    access_mode?: "shared_read" | "exclusive_write" | null;
                    cwd?: string | null;
                    execution_root?: string | null;
                    execution_root_id?: string | null;
                    is_active: boolean;
                    /** @enum {string|null} */
                    projection_kind?: "canonical_root" | "git_worktree_root" | null;
                    repo_name?: string | null;
                    workspace_alias?: string | null;
                    workspace_anchor?: string | null;
                    workspace_id: string;
                    worktree?: {
                        branch?: string | null;
                        original_branch?: string | null;
                        original_cwd?: string | null;
                        path?: string | null;
                    } | null;
                }[];
            };
        };
        /** @description Baseline request DTO schema. Per-field schemas will be tightened as HTTP envelope and DTO contracts stabilize. */
        AttachWorkspaceRequest: {
            [key: string]: unknown;
        };
        /** BatchGetMessagesRequest */
        BatchGetMessagesRequest: {
            /** @default [] */
            message_ids: string[];
        };
        BatchGetMessagesResponse: {
            messages: components["schemas"]["JsonValue"][];
            missing_message_ids?: string[];
        };
        /** BatchGetTranscriptEntriesRequest */
        BatchGetTranscriptEntriesRequest: {
            /** @default [] */
            entry_ids: string[];
        };
        BatchGetTranscriptEntriesResponse: {
            entries: components["schemas"]["JsonValue"][];
            missing_entry_ids?: string[];
        };
        /** BriefRecord */
        BriefRecord: {
            agent_id: string;
            attachments?: {
                kind: string;
                name: string;
                uri?: string | null;
                value?: unknown;
            }[] | null;
            /**
             * @default {
             *       "kind": "inline"
             *     }
             */
            content_source: {
                entry_id: string;
                /** @constant */
                kind: "transcript_entry";
                /**
                 * @default derived_from
                 * @enum {string}
                 */
                relation: "derived_from" | "finalizes" | "excerpt";
            } | {
                /** @constant */
                kind: "inline";
            };
            /** Format: date-time */
            created_at: string;
            finalizes_assistant_round_id?: string | null;
            id: string;
            /** @enum {string} */
            kind: "ack" | "result" | "failure";
            related_message_id?: string | null;
            related_task_id?: string | null;
            text: string;
            turn_id?: string | null;
            /** Format: uint64 */
            turn_index?: number | null;
            work_item_id?: string | null;
            /** @default agent_home */
            workspace_id: string;
        };
        /** @description Raw callback request body. JSON and text bodies are parsed; other content types are represented internally as base64 JSON. */
        CallbackBody: unknown;
        /** CancelTimerRequest */
        CancelTimerRequest: {
            /** @enum {string|null} */
            authority_class?: "operator_instruction" | "runtime_instruction" | "integration_signal" | "external_evidence" | null;
        };
        /** CheckSkillRequest */
        CheckSkillRequest: {
            name?: string | null;
        };
        /** @description Baseline request DTO schema. Per-field schemas will be tightened as HTTP envelope and DTO contracts stabilize. */
        CheckTemplateRequest: {
            [key: string]: unknown;
        };
        /** @description Baseline request DTO schema. Per-field schemas will be tightened as HTTP envelope and DTO contracts stabilize. */
        ClearAgentModelRequest: {
            [key: string]: unknown;
        };
        /** CompleteWorkItemRequest */
        CompleteWorkItemRequest: {
            /** @enum {string|null} */
            authority_class?: "operator_instruction" | "runtime_instruction" | "integration_signal" | "external_evidence" | null;
        };
        /** @description Baseline request DTO schema. Per-field schemas will be tightened as HTTP envelope and DTO contracts stabilize. */
        ControlPromptRequest: {
            [key: string]: unknown;
        };
        /** @description Baseline request DTO schema. Per-field schemas will be tightened as HTTP envelope and DTO contracts stabilize. */
        ControlRequest: {
            [key: string]: unknown;
        };
        /** @description Baseline request DTO schema. Per-field schemas will be tightened as HTTP envelope and DTO contracts stabilize. */
        ControlWakeRequest: {
            [key: string]: unknown;
        };
        /** @description Baseline request DTO schema. Per-field schemas will be tightened as HTTP envelope and DTO contracts stabilize. */
        CreateAgentRequest: {
            [key: string]: unknown;
        };
        /** @description Baseline request DTO schema. Per-field schemas will be tightened as HTTP envelope and DTO contracts stabilize. */
        CreateCommandTaskRequest: {
            [key: string]: unknown;
        };
        CreateJobRequest: {
            /** @enum {string} */
            kind: "skill.install" | "skill.update";
            params: components["schemas"]["AddSkillRequest"];
        };
        /** CreateTimerRequest */
        CreateTimerRequest: {
            /** @enum {string|null} */
            authority_class?: "operator_instruction" | "runtime_instruction" | "integration_signal" | "external_evidence" | null;
            /** Format: uint64 */
            duration_ms: number;
            /** Format: uint64 */
            interval_ms?: number | null;
            summary?: string | null;
        };
        /** @description Baseline request DTO schema. Per-field schemas will be tightened as HTTP envelope and DTO contracts stabilize. */
        CreateWorkItemRequest: {
            [key: string]: unknown;
        };
        /** @description Baseline request DTO schema. Per-field schemas will be tightened as HTTP envelope and DTO contracts stabilize. */
        DebugPromptRequest: {
            [key: string]: unknown;
        };
        /** @description Baseline request DTO schema. Per-field schemas will be tightened as HTTP envelope and DTO contracts stabilize. */
        DetachWorkspaceRequest: {
            [key: string]: unknown;
        };
        /** @description Baseline request DTO schema. Per-field schemas will be tightened as HTTP envelope and DTO contracts stabilize. */
        DisableSkillRequest: {
            [key: string]: unknown;
        };
        /** @description Baseline request DTO schema. Per-field schemas will be tightened as HTTP envelope and DTO contracts stabilize. */
        EnableSkillRequest: {
            [key: string]: unknown;
        };
        /** @description Baseline request DTO schema. Per-field schemas will be tightened as HTTP envelope and DTO contracts stabilize. */
        EnqueueRequest: {
            [key: string]: unknown;
        };
        ErrorResponse: {
            after_seq?: number;
            agent_id?: string;
            code?: string;
            error: string;
            event_seq?: number;
            hint?: string;
            /** @constant */
            ok?: false;
        } & {
            [key: string]: unknown;
        };
        /** @description Baseline request DTO schema. Per-field schemas will be tightened as HTTP envelope and DTO contracts stabilize. */
        ExitWorkspaceRequest: {
            [key: string]: unknown;
        };
        GenericJsonPayload: components["schemas"]["JsonValue"];
        /** @description Baseline request DTO schema. Per-field schemas will be tightened as HTTP envelope and DTO contracts stabilize. */
        IncomingOrigin: {
            [key: string]: unknown;
        };
        /** @description Baseline request DTO schema. Per-field schemas will be tightened as HTTP envelope and DTO contracts stabilize. */
        InstallSkillRequest: {
            [key: string]: unknown;
        };
        /** @description Baseline request DTO schema. Per-field schemas will be tightened as HTTP envelope and DTO contracts stabilize. */
        InstallTemplateRequest: {
            [key: string]: unknown;
        };
        JobResponse: {
            job: {
                /** Format: date-time */
                created_at: string;
                error?: string;
                id: string;
                items: {
                    [key: string]: unknown;
                }[];
                kind: string;
                phase: string;
                progress: {
                    [key: string]: unknown;
                };
                result?: {
                    [key: string]: unknown;
                };
                /** @enum {string} */
                status: "queued" | "running" | "completed" | "failed";
                summary: string;
                /** Format: date-time */
                updated_at: string;
            } & {
                [key: string]: unknown;
            };
            ok: boolean;
        };
        /** @description Arbitrary JSON value. Used as a conservative baseline for routes whose DTO is not yet stabilized. */
        JsonValue: unknown;
        /** MemoryGetRequest */
        MemoryGetRequest: {
            /** Format: uint */
            max_chars?: number | null;
            source_ref: string;
        };
        /** MemoryGetResult */
        MemoryGetResult: {
            agent_id: string;
            content: string;
            kind: string;
            metadata: unknown;
            scope_kind: string;
            source_path?: string | null;
            source_ref: string;
            title: string;
            truncated: boolean;
            /** Format: date-time */
            updated_at: string;
            workspace_id?: string | null;
        };
        /** ModelConfigMigrationReport */
        ModelConfigMigrationReport: {
            agent_state: {
                backup_path?: string | null;
                changed: boolean;
                fields: {
                    current: string;
                    error?: string | null;
                    location: string;
                    proposed?: string | null;
                    /** @enum {string} */
                    status: "canonical" | "legacy" | "invalid" | "ambiguous";
                }[];
            };
            changed: boolean;
            config: {
                backup_path?: string | null;
                changed: boolean;
                fields: {
                    current: string;
                    error?: string | null;
                    location: string;
                    proposed?: string | null;
                    /** @enum {string} */
                    status: "canonical" | "legacy" | "invalid" | "ambiguous";
                }[];
            };
            config_file_path: string;
            ok: boolean;
            runtime_db_path: string;
            write: boolean;
        };
        /** ModelConfigMigrationRequest */
        ModelConfigMigrationRequest: {
            /** @default false */
            write: boolean;
        };
        /** @description Baseline request DTO schema. Per-field schemas will be tightened as HTTP envelope and DTO contracts stabilize. */
        OperatorIngressRequest: {
            [key: string]: unknown;
        };
        /** @description Baseline request DTO schema. Per-field schemas will be tightened as HTTP envelope and DTO contracts stabilize. */
        OperatorTransportBindingRequest: {
            [key: string]: unknown;
        };
        /** PerformanceDiagnosticsSnapshot */
        PerformanceDiagnosticsSnapshot: {
            captured_at: string;
            db: {
                /** Format: double */
                avg_bytes?: number | null;
                /** Format: double */
                avg_ms: number;
                /** Format: uint64 */
                count: number;
                /** Format: uint64 */
                max_ms: number;
                name: string;
                /** Format: uint64 */
                total_bytes?: number | null;
                /** Format: uint64 */
                total_ms: number;
            }[];
            http: {
                /** Format: double */
                avg_bytes?: number | null;
                /** Format: double */
                avg_ms: number;
                /** Format: uint64 */
                count: number;
                /** Format: uint64 */
                max_ms: number;
                name: string;
                /** Format: uint64 */
                total_bytes?: number | null;
                /** Format: uint64 */
                total_ms: number;
            }[];
            /** Format: uint64 */
            process_uptime_ms: number;
            projections: {
                /** Format: double */
                avg_bytes?: number | null;
                /** Format: double */
                avg_ms: number;
                /** Format: uint64 */
                count: number;
                /** Format: uint64 */
                max_ms: number;
                name: string;
                /** Format: uint64 */
                total_bytes?: number | null;
                /** Format: uint64 */
                total_ms: number;
            }[];
            provider: {
                /** Format: double */
                avg_bytes?: number | null;
                /** Format: double */
                avg_ms: number;
                /** Format: uint64 */
                count: number;
                /** Format: uint64 */
                max_ms: number;
                name: string;
                /** Format: uint64 */
                total_bytes?: number | null;
                /** Format: uint64 */
                total_ms: number;
            }[];
            scheduler: {
                /** Format: double */
                avg_bytes?: number | null;
                /** Format: double */
                avg_ms: number;
                /** Format: uint64 */
                count: number;
                /** Format: uint64 */
                max_ms: number;
                name: string;
                /** Format: uint64 */
                total_bytes?: number | null;
                /** Format: uint64 */
                total_ms: number;
            }[];
            turn: {
                /** Format: double */
                avg_bytes?: number | null;
                /** Format: double */
                avg_ms: number;
                /** Format: uint64 */
                count: number;
                /** Format: uint64 */
                max_ms: number;
                name: string;
                /** Format: uint64 */
                total_bytes?: number | null;
                /** Format: uint64 */
                total_ms: number;
            }[];
        };
        /** PickWorkItemRequest */
        PickWorkItemRequest: {
            /** @enum {string|null} */
            authority_class?: "operator_instruction" | "runtime_instruction" | "integration_signal" | "external_evidence" | null;
            /** @default false */
            clear_blocker: boolean;
            reason?: string | null;
        };
        /** PickWorkItemResponse */
        PickWorkItemResponse: {
            current_work_item: {
                agent_id: string;
                blocked_by?: string | null;
                /** Format: date-time */
                created_at: string;
                id: string;
                objective: string;
                plan_artifact?: {
                    /** Format: uint64 */
                    bytes: number;
                    hash: string;
                    /** @default  */
                    owner_agent_id: string;
                    path: string;
                    preview: string;
                    preview_complete: boolean;
                    /** @default  */
                    relative_path: string;
                    /** Format: date-time */
                    updated_at: string;
                    workspace_alias?: string | null;
                    /** @default agent_home */
                    workspace_id: string;
                } | null;
                /** @enum {string} */
                plan_status: "draft" | "ready" | "needs_input";
                /** Format: date-time */
                recheck_at?: string | null;
                /** Format: date-time */
                recheck_consumed_at?: string | null;
                result_brief_id?: string | null;
                result_summary?: string | null;
                /**
                 * Format: uint64
                 * @default 1
                 */
                revision: number;
                /** @enum {string} */
                state: "open" | "completed";
                todo_list?: {
                    /** @enum {string} */
                    state: "pending" | "in_progress" | "completed";
                    text: string;
                }[];
                turn_id?: string | null;
                /** Format: date-time */
                updated_at: string;
                work_refs?: {
                    /** @enum {string} */
                    kind: "file" | "tool_execution" | "issue" | "pr" | "url" | "memory" | "task" | "wait" | "workspace" | "other";
                    /** Format: date-time */
                    last_seen_at: string;
                    metadata?: {
                        [key: string]: unknown;
                    };
                    reason: string;
                    ref: string;
                    source_ref?: string | null;
                    /** @enum {string} */
                    status: "active" | "resolved" | "stale" | "archived";
                    title?: string | null;
                }[];
                /** @default agent_home */
                workspace_id: string;
            };
            current_work_item_id: string;
            previous_work_item?: {
                agent_id: string;
                blocked_by?: string | null;
                /** Format: date-time */
                created_at: string;
                id: string;
                objective: string;
                plan_artifact?: {
                    /** Format: uint64 */
                    bytes: number;
                    hash: string;
                    /** @default  */
                    owner_agent_id: string;
                    path: string;
                    preview: string;
                    preview_complete: boolean;
                    /** @default  */
                    relative_path: string;
                    /** Format: date-time */
                    updated_at: string;
                    workspace_alias?: string | null;
                    /** @default agent_home */
                    workspace_id: string;
                } | null;
                /** @enum {string} */
                plan_status: "draft" | "ready" | "needs_input";
                /** Format: date-time */
                recheck_at?: string | null;
                /** Format: date-time */
                recheck_consumed_at?: string | null;
                result_brief_id?: string | null;
                result_summary?: string | null;
                /**
                 * Format: uint64
                 * @default 1
                 */
                revision: number;
                /** @enum {string} */
                state: "open" | "completed";
                todo_list?: {
                    /** @enum {string} */
                    state: "pending" | "in_progress" | "completed";
                    text: string;
                }[];
                turn_id?: string | null;
                /** Format: date-time */
                updated_at: string;
                work_refs?: {
                    /** @enum {string} */
                    kind: "file" | "tool_execution" | "issue" | "pr" | "url" | "memory" | "task" | "wait" | "workspace" | "other";
                    /** Format: date-time */
                    last_seen_at: string;
                    metadata?: {
                        [key: string]: unknown;
                    };
                    reason: string;
                    ref: string;
                    source_ref?: string | null;
                    /** @enum {string} */
                    status: "active" | "resolved" | "stale" | "archived";
                    title?: string | null;
                }[];
                /** @default agent_home */
                workspace_id: string;
            } | null;
            transition: {
                blocker_cleared: boolean;
                cancelled_wait_condition_ids?: string[];
                current_focus_mode: string;
                /** @enum {string} */
                current_readiness: "runnable" | "yielded" | "waiting_for_operator" | "blocked" | "completed";
                current_work_item_id: string;
                /** @enum {string|null} */
                previous_readiness?: "runnable" | "yielded" | "waiting_for_operator" | "blocked" | "completed" | null;
                previous_work_item_id?: string | null;
                reason?: string | null;
                switch_kind: string;
                warnings?: {
                    code: string;
                    message: string;
                }[];
            };
        };
        /** ReconcileSkillRequest */
        ReconcileSkillRequest: {
            name?: string | null;
        };
        /** RefreshCatalogRequest */
        RefreshCatalogRequest: Record<string, never>;
        /** @description Baseline request DTO schema. Per-field schemas will be tightened as HTTP envelope and DTO contracts stabilize. */
        RemoveSkillRequest: {
            [key: string]: unknown;
        };
        /** @description Baseline request DTO schema. Per-field schemas will be tightened as HTTP envelope and DTO contracts stabilize. */
        RemoveTemplateRequest: {
            [key: string]: unknown;
        };
        /** RuntimeConfigReadResponse */
        RuntimeConfigReadResponse: {
            config_file_path: string;
            ok: boolean;
            runtime_surface: {
                /** @default [] */
                available_search_provider_kinds: {
                    capabilities: {
                        /** @enum {string} */
                        auth: "none" | "api_key" | "native_provider" | "self_hosted";
                        /** @enum {string} */
                        cost_class: "free" | "self_hosted" | "paid" | "provider_metered";
                        /** Format: uint16 */
                        default_priority: number;
                        /** @enum {string} */
                        quality_hint: "html_fallback" | "keyword" | "semantic" | "research" | "native";
                        /** @enum {string} */
                        status: "supported" | "unsupported" | "native_only";
                        supports_domain_filter: boolean;
                        supports_freshness: boolean;
                        supports_full_content: boolean;
                        supports_native_citations: boolean;
                        supports_region_or_language: boolean;
                    };
                    kind: string;
                }[];
                /** Format: uint32 */
                default_tool_output_tokens: number;
                disable_provider_fallback: boolean;
                image_generation_default?: string | null;
                /** Format: uint32 */
                max_tool_output_tokens: number;
                model_catalog: string[];
                model_default: string;
                model_fallbacks: string[];
                providers: {
                    api_key_supported: boolean;
                    base_url: string;
                    configured_in_config: boolean;
                    credential_configured: boolean;
                    credential_env?: string | null;
                    credential_external?: string | null;
                    credential_kind: string;
                    credential_profile?: string | null;
                    credential_source: string;
                    id: string;
                    oauth_supported: boolean;
                    transport: string;
                }[];
                /** Format: uint32 */
                runtime_max_output_tokens: number;
                unknown_model_fallback_configured: boolean;
                vision_default?: string | null;
                web_search: {
                    builtin_provider_enabled: boolean;
                    enabled: boolean;
                    /** Format: uint */
                    max_provider_attempts: number;
                    /** Format: uint */
                    max_results: number;
                    mode: string;
                    provider: string;
                    providers: string[];
                };
                web_search_providers: {
                    base_url?: string | null;
                    credential_configured: boolean;
                    credential_profile?: string | null;
                    id: string;
                    kind: string;
                }[];
            };
        };
        /** RuntimeConfigUpdateRequest */
        RuntimeConfigUpdateRequest: {
            updates: {
                key: string;
                /** @default false */
                unset: boolean;
                value?: unknown;
            }[];
        };
        /** RuntimeConfigUpdateResponse */
        RuntimeConfigUpdateResponse: {
            changed: boolean;
            config_file_path: string;
            ok: boolean;
            results: {
                /** @enum {string} */
                effect: "accepted_requires_restart" | "accepted_reloaded" | "rejected";
                key: string;
                reason: string;
            }[];
            runtime_surface: {
                /** @default [] */
                available_search_provider_kinds: {
                    capabilities: {
                        /** @enum {string} */
                        auth: "none" | "api_key" | "native_provider" | "self_hosted";
                        /** @enum {string} */
                        cost_class: "free" | "self_hosted" | "paid" | "provider_metered";
                        /** Format: uint16 */
                        default_priority: number;
                        /** @enum {string} */
                        quality_hint: "html_fallback" | "keyword" | "semantic" | "research" | "native";
                        /** @enum {string} */
                        status: "supported" | "unsupported" | "native_only";
                        supports_domain_filter: boolean;
                        supports_freshness: boolean;
                        supports_full_content: boolean;
                        supports_native_citations: boolean;
                        supports_region_or_language: boolean;
                    };
                    kind: string;
                }[];
                /** Format: uint32 */
                default_tool_output_tokens: number;
                disable_provider_fallback: boolean;
                image_generation_default?: string | null;
                /** Format: uint32 */
                max_tool_output_tokens: number;
                model_catalog: string[];
                model_default: string;
                model_fallbacks: string[];
                providers: {
                    api_key_supported: boolean;
                    base_url: string;
                    configured_in_config: boolean;
                    credential_configured: boolean;
                    credential_env?: string | null;
                    credential_external?: string | null;
                    credential_kind: string;
                    credential_profile?: string | null;
                    credential_source: string;
                    id: string;
                    oauth_supported: boolean;
                    transport: string;
                }[];
                /** Format: uint32 */
                runtime_max_output_tokens: number;
                unknown_model_fallback_configured: boolean;
                vision_default?: string | null;
                web_search: {
                    builtin_provider_enabled: boolean;
                    enabled: boolean;
                    /** Format: uint */
                    max_provider_attempts: number;
                    /** Format: uint */
                    max_results: number;
                    mode: string;
                    provider: string;
                    providers: string[];
                };
                web_search_providers: {
                    base_url?: string | null;
                    credential_configured: boolean;
                    credential_profile?: string | null;
                    id: string;
                    kind: string;
                }[];
            };
        };
        /** SearchRequest */
        SearchRequest: {
            /** @default null */
            agent_ids: string[] | null;
            /** @default false */
            include_all_workspaces: boolean;
            /** Format: uint */
            limit?: number | null;
            query: string;
            /** @default [] */
            types: string[];
        };
        /** SearchResponse */
        SearchResponse: {
            index_status: {
                consumption_was_limited?: boolean;
                /** Format: int64 */
                cursor: number;
                freshness: string;
                /** Format: int64 */
                high_watermark: number;
                indexing_needed?: boolean;
                /** Format: int64 */
                lag: number;
                /** Format: date-time */
                last_indexed_at?: string | null;
                results_may_be_incomplete?: boolean;
                /** Format: uint */
                skipped_error_count?: number;
            };
            /** Format: uint */
            limit: number;
            query: string;
            results: {
                agent_id: string;
                kind: string;
                metadata: unknown;
                scope_kind: string;
                /** Format: double */
                score: number;
                snippet: string;
                source_path?: string | null;
                source_ref: string;
                title: string;
                /** Format: date-time */
                updated_at: string;
                workspace_id?: string | null;
            }[];
        };
        /** @description Baseline request DTO schema. Per-field schemas will be tightened as HTTP envelope and DTO contracts stabilize. */
        SetAgentModelRequest: {
            [key: string]: unknown;
        };
        /** @description Baseline request DTO schema. Per-field schemas will be tightened as HTTP envelope and DTO contracts stabilize. */
        SetCredentialRequest: {
            [key: string]: unknown;
        };
        /** SlimTaskDto */
        SlimTaskDto: {
            agent_id: string;
            /** Format: date-time */
            created_at: string;
            id: string;
            /** @enum {string} */
            kind: "command_task" | "child_agent_task" | "sleep_job" | "subagent_task" | "worktree_subagent_task";
            parent_message_id?: string | null;
            /** @enum {string} */
            status: "queued" | "running" | "cancelling" | "completed" | "failed" | "cancelled" | "interrupted";
            summary?: string | null;
            /** Format: date-time */
            updated_at: string;
            work_item_id?: string | null;
        };
        /** SlimWorkItemDto */
        SlimWorkItemDto: {
            agent_id: string;
            blocked_by?: string | null;
            /** @enum {string} */
            candidate_class: "current_runnable" | "triggered_blocked" | "queued_runnable" | "waiting_for_operator" | "yielded" | "blocked" | "completed_recent";
            /** Format: date-time */
            created_at: string;
            current_todo?: {
                /** @enum {string} */
                state: "pending" | "in_progress" | "completed";
                text: string;
            } | null;
            /** @enum {string} */
            focus: "current" | "queued" | "yielded" | "blocked" | "completed";
            id: string;
            is_current: boolean;
            is_runnable: boolean;
            objective: string;
            /** @enum {string} */
            plan_status: "draft" | "ready" | "needs_input";
            /** @enum {string} */
            readiness: "runnable" | "yielded" | "waiting_for_operator" | "blocked" | "completed";
            /** @enum {string} */
            reason_code: "completed" | "continuation_yielded" | "active_task_wait" | "active_operator_wait" | "active_timer_wait" | "active_external_wait" | "active_system_wait" | "manual_blocker" | "plan_needs_input" | "runnable";
            /** Format: date-time */
            recheck_at?: string | null;
            result_brief_id?: string | null;
            result_summary?: string | null;
            /** Format: uint64 */
            revision: number;
            /** @enum {string} */
            scheduling_state: "runnable" | "yielded_to_work_item" | "waiting_operator" | "waiting_task" | "waiting_external" | "waiting_timer" | "waiting_system" | "blocked" | "completed";
            /** @enum {string} */
            state: "open" | "completed";
            turn_id?: string | null;
            /** Format: date-time */
            updated_at: string;
            workspace_id: string;
        };
        /** SyncTemplateRemoteSourcesRequest */
        SyncTemplateRemoteSourcesRequest: {
            /**
             * @description Force a refresh even if a later TTL policy would consider the cache
             *      fresh. Currently accepted for forward compatibility.
             */
            force?: boolean;
            /**
             * @description Optional configured source id. When omitted, all enabled remote sources
             *      are synchronized.
             */
            source_id?: string | null;
        };
        /** @description Baseline request DTO schema. Per-field schemas will be tightened as HTTP envelope and DTO contracts stabilize. */
        TaskInputRequest: {
            [key: string]: unknown;
        };
        /** TaskInputResult */
        TaskInputResult: {
            accepted_input: boolean;
            /** Format: uint64 */
            bytes_written?: number | null;
            input_target?: string | null;
            summary_text?: string | null;
            task: {
                child_agent_id?: string | null;
                child_observability?: {
                    /** @enum {string|null} */
                    blocked_reason?: "managed_task_queued" | "managed_task_running" | "managed_task_cancelling" | "awaiting_managed_task" | null;
                    current_work_item_id?: string | null;
                    last_progress_brief?: string | null;
                    last_result_brief?: string | null;
                    /** @enum {string} */
                    phase: "running" | "blocked" | "waiting" | "terminal";
                    /** @enum {string|null} */
                    waiting_reason?: "awaiting_operator_input" | "awaiting_external_change" | "awaiting_task_result" | "awaiting_timer" | null;
                    work_summary?: string | null;
                } | null;
                child_supervision?: {
                    child_agent_id: string;
                    child_work_item_id?: string | null;
                    cleanup_owner: string;
                    cleanup_status?: string | null;
                    delegation_id?: string | null;
                    followup_target: string;
                    parent_agent_id: string;
                    parent_work_item_id?: string | null;
                    supervision_task_id: string;
                    /** @enum {string|null} */
                    workspace_mode?: "inherit" | "worktree" | null;
                    worktree?: {
                        actual_branch?: string | null;
                        auto_cleaned_up?: boolean | null;
                        branch_cleanup_error?: string | null;
                        branch_cleanup_status?: string | null;
                        changed_files?: string[];
                        cleanup_error?: string | null;
                        cleanup_reason?: string | null;
                        cleanup_status?: string | null;
                        original_branch?: string | null;
                        original_cwd?: string | null;
                        projection_kind?: string | null;
                        retained_for_review?: boolean | null;
                        worktree_branch?: string | null;
                        worktree_path?: string | null;
                    } | null;
                } | null;
                command?: {
                    accepts_input?: boolean | null;
                    cmd?: string | null;
                    cmd_digest?: string | null;
                    /** Format: int32 */
                    exit_status?: number | null;
                    input_target?: string | null;
                    login?: boolean | null;
                    output_path?: string | null;
                    promoted_from_exec_command?: boolean | null;
                    result_summary?: string | null;
                    shell?: string | null;
                    terminal_reentry?: boolean | null;
                    tty?: boolean | null;
                    workdir?: string | null;
                } | null;
                /** Format: date-time */
                created_at: string;
                kind: string;
                parent_message_id?: string | null;
                /** @enum {string} */
                status: "queued" | "running" | "cancelling" | "completed" | "failed" | "cancelled" | "interrupted";
                summary?: string | null;
                task_id: string;
                token_usage?: {
                    last_turn?: {
                        /** Format: uint64 */
                        input_tokens: number;
                        /** Format: uint64 */
                        output_tokens: number;
                        /** Format: uint64 */
                        total_tokens: number;
                    } | null;
                    total: {
                        /** Format: uint64 */
                        input_tokens: number;
                        /** Format: uint64 */
                        output_tokens: number;
                        /** Format: uint64 */
                        total_tokens: number;
                    };
                    /** Format: uint64 */
                    total_model_rounds: number;
                } | null;
                /** Format: date-time */
                updated_at: string;
                /** @enum {string} */
                wait_policy: "background";
            };
        };
        /** TaskOutputResult */
        TaskOutputResult: {
            /** @enum {string} */
            retrieval_status: "success" | "timeout" | "not_ready";
            task: {
                artifacts?: {
                    path: string;
                }[];
                child_supervision?: {
                    child_agent_id: string;
                    child_work_item_id?: string | null;
                    cleanup_owner: string;
                    cleanup_status?: string | null;
                    delegation_id?: string | null;
                    followup_target: string;
                    parent_agent_id: string;
                    parent_work_item_id?: string | null;
                    supervision_task_id: string;
                    /** @enum {string|null} */
                    workspace_mode?: "inherit" | "worktree" | null;
                    worktree?: {
                        actual_branch?: string | null;
                        auto_cleaned_up?: boolean | null;
                        branch_cleanup_error?: string | null;
                        branch_cleanup_status?: string | null;
                        changed_files?: string[];
                        cleanup_error?: string | null;
                        cleanup_reason?: string | null;
                        cleanup_status?: string | null;
                        original_branch?: string | null;
                        original_cwd?: string | null;
                        projection_kind?: string | null;
                        retained_for_review?: boolean | null;
                        worktree_branch?: string | null;
                        worktree_path?: string | null;
                    } | null;
                } | null;
                /** Format: int32 */
                exit_status?: number | null;
                failure_artifact?: {
                    /** @enum {string} */
                    category: "transport" | "protocol" | "runtime" | "task" | "unknown";
                    /** Format: int32 */
                    exit_status?: number | null;
                    kind: string;
                    metadata?: {
                        [key: string]: string;
                    };
                    model_ref?: string | null;
                    provider?: string | null;
                    source_chain?: string[];
                    /** Format: uint16 */
                    status?: number | null;
                    summary: string;
                    task_id?: string | null;
                } | null;
                kind: string;
                /** Format: uint */
                output_artifact?: number | null;
                output_preview: string;
                output_truncated: boolean;
                result_summary?: string | null;
                /** @enum {string} */
                status: "queued" | "running" | "cancelling" | "completed" | "failed" | "cancelled" | "interrupted";
                summary?: string | null;
                task_id: string;
                token_usage?: {
                    last_turn?: {
                        /** Format: uint64 */
                        input_tokens: number;
                        /** Format: uint64 */
                        output_tokens: number;
                        /** Format: uint64 */
                        total_tokens: number;
                    } | null;
                    total: {
                        /** Format: uint64 */
                        input_tokens: number;
                        /** Format: uint64 */
                        output_tokens: number;
                        /** Format: uint64 */
                        total_tokens: number;
                    };
                    /** Format: uint64 */
                    total_model_rounds: number;
                } | null;
            };
        };
        /** TaskStatusSnapshot */
        TaskStatusSnapshot: {
            child_agent_id?: string | null;
            child_observability?: {
                /** @enum {string|null} */
                blocked_reason?: "managed_task_queued" | "managed_task_running" | "managed_task_cancelling" | "awaiting_managed_task" | null;
                current_work_item_id?: string | null;
                last_progress_brief?: string | null;
                last_result_brief?: string | null;
                /** @enum {string} */
                phase: "running" | "blocked" | "waiting" | "terminal";
                /** @enum {string|null} */
                waiting_reason?: "awaiting_operator_input" | "awaiting_external_change" | "awaiting_task_result" | "awaiting_timer" | null;
                work_summary?: string | null;
            } | null;
            child_supervision?: {
                child_agent_id: string;
                child_work_item_id?: string | null;
                cleanup_owner: string;
                cleanup_status?: string | null;
                delegation_id?: string | null;
                followup_target: string;
                parent_agent_id: string;
                parent_work_item_id?: string | null;
                supervision_task_id: string;
                /** @enum {string|null} */
                workspace_mode?: "inherit" | "worktree" | null;
                worktree?: {
                    actual_branch?: string | null;
                    auto_cleaned_up?: boolean | null;
                    branch_cleanup_error?: string | null;
                    branch_cleanup_status?: string | null;
                    changed_files?: string[];
                    cleanup_error?: string | null;
                    cleanup_reason?: string | null;
                    cleanup_status?: string | null;
                    original_branch?: string | null;
                    original_cwd?: string | null;
                    projection_kind?: string | null;
                    retained_for_review?: boolean | null;
                    worktree_branch?: string | null;
                    worktree_path?: string | null;
                } | null;
            } | null;
            command?: {
                accepts_input?: boolean | null;
                cmd?: string | null;
                cmd_digest?: string | null;
                /** Format: int32 */
                exit_status?: number | null;
                input_target?: string | null;
                login?: boolean | null;
                output_path?: string | null;
                promoted_from_exec_command?: boolean | null;
                result_summary?: string | null;
                shell?: string | null;
                terminal_reentry?: boolean | null;
                tty?: boolean | null;
                workdir?: string | null;
            } | null;
            /** Format: date-time */
            created_at: string;
            kind: string;
            parent_message_id?: string | null;
            /** @enum {string} */
            status: "queued" | "running" | "cancelling" | "completed" | "failed" | "cancelled" | "interrupted";
            summary?: string | null;
            task_id: string;
            token_usage?: {
                last_turn?: {
                    /** Format: uint64 */
                    input_tokens: number;
                    /** Format: uint64 */
                    output_tokens: number;
                    /** Format: uint64 */
                    total_tokens: number;
                } | null;
                total: {
                    /** Format: uint64 */
                    input_tokens: number;
                    /** Format: uint64 */
                    output_tokens: number;
                    /** Format: uint64 */
                    total_tokens: number;
                };
                /** Format: uint64 */
                total_model_rounds: number;
            } | null;
            /** Format: date-time */
            updated_at: string;
            /** @enum {string} */
            wait_policy: "background";
        };
        /** @description Baseline request DTO schema. Per-field schemas will be tightened as HTTP envelope and DTO contracts stabilize. */
        TaskStopRequest: {
            [key: string]: unknown;
        };
        /** TaskStopResult */
        TaskStopResult: {
            force_stop_requested: boolean;
            stop_requested: boolean;
            summary_text?: string | null;
            task: {
                child_agent_id?: string | null;
                child_observability?: {
                    /** @enum {string|null} */
                    blocked_reason?: "managed_task_queued" | "managed_task_running" | "managed_task_cancelling" | "awaiting_managed_task" | null;
                    current_work_item_id?: string | null;
                    last_progress_brief?: string | null;
                    last_result_brief?: string | null;
                    /** @enum {string} */
                    phase: "running" | "blocked" | "waiting" | "terminal";
                    /** @enum {string|null} */
                    waiting_reason?: "awaiting_operator_input" | "awaiting_external_change" | "awaiting_task_result" | "awaiting_timer" | null;
                    work_summary?: string | null;
                } | null;
                child_supervision?: {
                    child_agent_id: string;
                    child_work_item_id?: string | null;
                    cleanup_owner: string;
                    cleanup_status?: string | null;
                    delegation_id?: string | null;
                    followup_target: string;
                    parent_agent_id: string;
                    parent_work_item_id?: string | null;
                    supervision_task_id: string;
                    /** @enum {string|null} */
                    workspace_mode?: "inherit" | "worktree" | null;
                    worktree?: {
                        actual_branch?: string | null;
                        auto_cleaned_up?: boolean | null;
                        branch_cleanup_error?: string | null;
                        branch_cleanup_status?: string | null;
                        changed_files?: string[];
                        cleanup_error?: string | null;
                        cleanup_reason?: string | null;
                        cleanup_status?: string | null;
                        original_branch?: string | null;
                        original_cwd?: string | null;
                        projection_kind?: string | null;
                        retained_for_review?: boolean | null;
                        worktree_branch?: string | null;
                        worktree_path?: string | null;
                    } | null;
                } | null;
                command?: {
                    accepts_input?: boolean | null;
                    cmd?: string | null;
                    cmd_digest?: string | null;
                    /** Format: int32 */
                    exit_status?: number | null;
                    input_target?: string | null;
                    login?: boolean | null;
                    output_path?: string | null;
                    promoted_from_exec_command?: boolean | null;
                    result_summary?: string | null;
                    shell?: string | null;
                    terminal_reentry?: boolean | null;
                    tty?: boolean | null;
                    workdir?: string | null;
                } | null;
                /** Format: date-time */
                created_at: string;
                kind: string;
                parent_message_id?: string | null;
                /** @enum {string} */
                status: "queued" | "running" | "cancelling" | "completed" | "failed" | "cancelled" | "interrupted";
                summary?: string | null;
                task_id: string;
                token_usage?: {
                    last_turn?: {
                        /** Format: uint64 */
                        input_tokens: number;
                        /** Format: uint64 */
                        output_tokens: number;
                        /** Format: uint64 */
                        total_tokens: number;
                    } | null;
                    total: {
                        /** Format: uint64 */
                        input_tokens: number;
                        /** Format: uint64 */
                        output_tokens: number;
                        /** Format: uint64 */
                        total_tokens: number;
                    };
                    /** Format: uint64 */
                    total_model_rounds: number;
                } | null;
                /** Format: date-time */
                updated_at: string;
                /** @enum {string} */
                wait_policy: "background";
            };
        };
        /** TimerRecord */
        TimerRecord: {
            agent_id: string;
            /** Format: date-time */
            created_at: string;
            /** Format: uint64 */
            duration_ms: number;
            /**
             * Format: uint64
             * @default 0
             */
            fire_count: number;
            id: string;
            /** Format: uint64 */
            interval_ms?: number | null;
            /**
             * Format: date-time
             * @default null
             */
            last_fired_at: string | null;
            /**
             * Format: date-time
             * @default null
             */
            next_fire_at: string | null;
            repeat: boolean;
            /**
             * @default active
             * @enum {string}
             */
            status: "active" | "completed" | "cancelled";
            summary?: string | null;
        };
        /** ToolExecutionArtifactContent */
        ToolExecutionArtifactContent: {
            /** Format: uint */
            artifact_index: number;
            content: string;
            /** Format: uint64 */
            size: number;
        };
        /** ToolExecutionRecord */
        ToolExecutionRecord: {
            agent_id: string;
            /** @enum {string} */
            authority_class: "operator_instruction" | "runtime_instruction" | "integration_signal" | "external_evidence";
            /**
             * Format: date-time
             * @default null
             */
            completed_at: string | null;
            /** Format: date-time */
            created_at: string;
            /**
             * Format: uint64
             * @default 0
             */
            duration_ms: number;
            id: string;
            input: unknown;
            invocation_surface?: string | null;
            output: unknown;
            /** @enum {string} */
            status: "success" | "error";
            summary: string;
            tool_name: string;
            turn_id?: string | null;
            /**
             * Format: uint64
             * @default 0
             */
            turn_index: number;
            work_item_id?: string | null;
        };
        /** @description Baseline request DTO schema. Per-field schemas will be tightened as HTTP envelope and DTO contracts stabilize. */
        UninstallSkillRequest: {
            [key: string]: unknown;
        };
        /** ReconcileSkillRequest */
        UpdateSkillRequest: {
            name?: string | null;
        };
        /** UpdateWorkItemRequest */
        UpdateWorkItemRequest: {
            /** @enum {string|null} */
            authority_class?: "operator_instruction" | "runtime_instruction" | "integration_signal" | "external_evidence" | null;
            blocked_by?: unknown;
            objective?: string | null;
            /** @enum {string|null} */
            plan_status?: "draft" | "ready" | "needs_input" | null;
            /** Format: uint64 */
            recheck_after?: number | null;
            todo_list?: {
                /** @enum {string} */
                state: "pending" | "in_progress" | "completed";
                text: string;
            }[] | null;
        };
        /** WorkItemRecord */
        WorkItemRecord: {
            agent_id: string;
            blocked_by?: string | null;
            /** Format: date-time */
            created_at: string;
            id: string;
            objective: string;
            plan_artifact?: {
                /** Format: uint64 */
                bytes: number;
                hash: string;
                /** @default  */
                owner_agent_id: string;
                path: string;
                preview: string;
                preview_complete: boolean;
                /** @default  */
                relative_path: string;
                /** Format: date-time */
                updated_at: string;
                workspace_alias?: string | null;
                /** @default agent_home */
                workspace_id: string;
            } | null;
            /** @enum {string} */
            plan_status: "draft" | "ready" | "needs_input";
            /** Format: date-time */
            recheck_at?: string | null;
            /** Format: date-time */
            recheck_consumed_at?: string | null;
            result_brief_id?: string | null;
            result_summary?: string | null;
            /**
             * Format: uint64
             * @default 1
             */
            revision: number;
            /** @enum {string} */
            state: "open" | "completed";
            todo_list?: {
                /** @enum {string} */
                state: "pending" | "in_progress" | "completed";
                text: string;
            }[];
            turn_id?: string | null;
            /** Format: date-time */
            updated_at: string;
            work_refs?: {
                /** @enum {string} */
                kind: "file" | "tool_execution" | "issue" | "pr" | "url" | "memory" | "task" | "wait" | "workspace" | "other";
                /** Format: date-time */
                last_seen_at: string;
                metadata?: {
                    [key: string]: unknown;
                };
                reason: string;
                ref: string;
                source_ref?: string | null;
                /** @enum {string} */
                status: "active" | "resolved" | "stale" | "archived";
                title?: string | null;
            }[];
            /** @default agent_home */
            workspace_id: string;
        };
    };
    responses: never;
    parameters: never;
    requestBodies: never;
    headers: never;
    pathItems: never;
}
export type $defs = Record<string, never>;
export interface operations {
    root: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Successful JSON response. Baseline schema is intentionally loose until per-route response DTO contracts are stabilized. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["JsonValue"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    listAgents: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Successful JSON response. Baseline schema is intentionally loose until per-route response DTO contracts are stabilized. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["JsonValue"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    agentBriefs: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Agent id. */
                agent_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Successful JSON response. Baseline schema is intentionally loose until per-route response DTO contracts are stabilized. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["JsonValue"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    agentBrief: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Agent id. */
                agent_id: string;
                /** @description Brief id. */
                brief_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Successful JSON response using a stable DTO schema. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["BriefRecord"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    enqueueAgent: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Agent id. */
                agent_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["EnqueueRequest"];
            };
        };
        responses: {
            /** @description Successful JSON response. Baseline schema is intentionally loose until per-route response DTO contracts are stabilized. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["JsonValue"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    agentEvents: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Agent id. */
                agent_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Successful JSON response. Baseline schema is intentionally loose until per-route response DTO contracts are stabilized. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["JsonValue"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    agentEventsStream: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Agent id. */
                agent_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Server-Sent Events stream. Each data frame contains a StreamEventEnvelope JSON object. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "text/event-stream": string;
                };
            };
            /** @description Client error before stream establishment. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    agentMessage: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Agent id. */
                agent_id: string;
                /** @description Message id. */
                message_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Successful JSON response. Baseline schema is intentionally loose until per-route response DTO contracts are stabilized. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["JsonValue"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    agentMessagesBatchGet: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Agent id. */
                agent_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["BatchGetMessagesRequest"];
            };
        };
        responses: {
            /** @description Successful JSON response using a stable DTO schema. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["BatchGetMessagesResponse"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    agentSkills: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Agent id. */
                agent_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Successful JSON response. Baseline schema is intentionally loose until per-route response DTO contracts are stabilized. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["JsonValue"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    agentState: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Agent id. */
                agent_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Successful JSON response using a stable DTO schema. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["AgentStateSnapshotDto"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    agentStatus: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Agent id. */
                agent_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Successful JSON response. Baseline schema is intentionally loose until per-route response DTO contracts are stabilized. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["JsonValue"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    agentTasks: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Agent id. */
                agent_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Successful JSON response. Baseline schema is intentionally loose until per-route response DTO contracts are stabilized. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["JsonValue"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    agentTaskStatus: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Agent id. */
                agent_id: string;
                /** @description Task id. */
                task_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Successful JSON response using a stable DTO schema. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["TaskStatusSnapshot"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    agentTaskOutput: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Agent id. */
                agent_id: string;
                /** @description Task id. */
                task_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Successful JSON response using a stable DTO schema. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["TaskOutputResult"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    agentTimers: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Agent id. */
                agent_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Successful JSON response. Baseline schema is intentionally loose until per-route response DTO contracts are stabilized. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["JsonValue"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    agentTimer: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Agent id. */
                agent_id: string;
                /** @description Timer id. */
                timer_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Successful JSON response. Baseline schema is intentionally loose until per-route response DTO contracts are stabilized. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["JsonValue"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    agentToolExecution: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Agent id. */
                agent_id: string;
                /** @description Tool execution id. */
                tool_execution_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Successful JSON response using a stable DTO schema. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ToolExecutionRecord"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    agentToolExecutionArtifact: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Agent id. */
                agent_id: string;
                /** @description Tool execution id. */
                tool_execution_id: string;
                /** @description Zero-based artifact index within the tool execution result. */
                artifact_index: number;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Successful JSON response using a stable DTO schema. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ToolExecutionArtifactContent"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    agentTranscript: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Agent id. */
                agent_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Successful JSON response. Baseline schema is intentionally loose until per-route response DTO contracts are stabilized. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["JsonValue"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    agentTranscriptEntry: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Agent id. */
                agent_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Successful JSON response. Baseline schema is intentionally loose until per-route response DTO contracts are stabilized. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["JsonValue"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    agentTranscriptBatchGet: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Agent id. */
                agent_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["BatchGetTranscriptEntriesRequest"];
            };
        };
        responses: {
            /** @description Successful JSON response using a stable DTO schema. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["BatchGetTranscriptEntriesResponse"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    agentWorkItems: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Agent id. */
                agent_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Successful JSON response. Baseline schema is intentionally loose until per-route response DTO contracts are stabilized. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["JsonValue"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    agentWorkItem: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Agent id. */
                agent_id: string;
                /** @description Work item id. */
                work_item_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Successful JSON response. Baseline schema is intentionally loose until per-route response DTO contracts are stabilized. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["JsonValue"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    agentWorktreeSummary: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Agent id. */
                agent_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Successful JSON response. Baseline schema is intentionally loose until per-route response DTO contracts are stabilized. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["JsonValue"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    startCodexDeviceLogin: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Successful JSON response. Baseline schema is intentionally loose until per-route response DTO contracts are stabilized. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["JsonValue"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    startOAuthDeviceLogin: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Successful JSON response. Baseline schema is intentionally loose until per-route response DTO contracts are stabilized. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["JsonValue"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    defaultBriefs: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Successful JSON response. Baseline schema is intentionally loose until per-route response DTO contracts are stabilized. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["JsonValue"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    callbackEnqueue: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Opaque callback capability token. This is a secret and must not be exposed in examples. */
                callback_token: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CallbackBody"];
            };
        };
        responses: {
            /** @description Successful JSON response. Baseline schema is intentionally loose until per-route response DTO contracts are stabilized. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["JsonValue"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    callbackWake: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Opaque callback capability token. This is a secret and must not be exposed in examples. */
                callback_token: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CallbackBody"];
            };
        };
        responses: {
            /** @description Successful JSON response. Baseline schema is intentionally loose until per-route response DTO contracts are stabilized. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["JsonValue"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    controlAgent: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Agent id. */
                agent_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["ControlRequest"];
            };
        };
        responses: {
            /** @description Successful JSON response. Baseline schema is intentionally loose until per-route response DTO contracts are stabilized. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["JsonValue"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    createAgent: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Agent id. */
                agent_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CreateAgentRequest"];
            };
        };
        responses: {
            /** @description Successful JSON response. Baseline schema is intentionally loose until per-route response DTO contracts are stabilized. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["JsonValue"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    abortCurrentRun: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Agent id. */
                agent_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["AbortCurrentRunRequest"];
            };
        };
        responses: {
            /** @description Successful JSON response. Baseline schema is intentionally loose until per-route response DTO contracts are stabilized. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["JsonValue"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    debugPrompt: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Agent id. */
                agent_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["DebugPromptRequest"];
            };
        };
        responses: {
            /** @description Successful JSON response. Baseline schema is intentionally loose until per-route response DTO contracts are stabilized. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["JsonValue"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    setAgentModel: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Agent id. */
                agent_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["SetAgentModelRequest"];
            };
        };
        responses: {
            /** @description Successful JSON response. Baseline schema is intentionally loose until per-route response DTO contracts are stabilized. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["JsonValue"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    clearAgentModel: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Agent id. */
                agent_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["ClearAgentModelRequest"];
            };
        };
        responses: {
            /** @description Successful JSON response. Baseline schema is intentionally loose until per-route response DTO contracts are stabilized. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["JsonValue"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    createOperatorTransportBinding: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Agent id. */
                agent_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["OperatorTransportBindingRequest"];
            };
        };
        responses: {
            /** @description Successful JSON response. Baseline schema is intentionally loose until per-route response DTO contracts are stabilized. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["JsonValue"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    operatorIngress: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Agent id. */
                agent_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["OperatorIngressRequest"];
            };
        };
        responses: {
            /** @description Successful JSON response. Baseline schema is intentionally loose until per-route response DTO contracts are stabilized. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["JsonValue"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    controlPrompt: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Agent id. */
                agent_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["ControlPromptRequest"];
            };
        };
        responses: {
            /** @description Successful JSON response. Baseline schema is intentionally loose until per-route response DTO contracts are stabilized. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["JsonValue"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    resetCallback: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Agent id. */
                agent_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Successful JSON response. Baseline schema is intentionally loose until per-route response DTO contracts are stabilized. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["JsonValue"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    disableSkill: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Agent id. */
                agent_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["DisableSkillRequest"];
            };
        };
        responses: {
            /** @description Successful JSON response. Baseline schema is intentionally loose until per-route response DTO contracts are stabilized. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["JsonValue"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    enableSkill: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Agent id. */
                agent_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["EnableSkillRequest"];
            };
        };
        responses: {
            /** @description Successful JSON response. Baseline schema is intentionally loose until per-route response DTO contracts are stabilized. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["JsonValue"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    installSkill: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Agent id. */
                agent_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["InstallSkillRequest"];
            };
        };
        responses: {
            /** @description Successful JSON response. Baseline schema is intentionally loose until per-route response DTO contracts are stabilized. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["JsonValue"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    uninstallSkill: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Agent id. */
                agent_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["UninstallSkillRequest"];
            };
        };
        responses: {
            /** @description Successful JSON response. Baseline schema is intentionally loose until per-route response DTO contracts are stabilized. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["JsonValue"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    createCommandTask: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Agent id. */
                agent_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CreateCommandTaskRequest"];
            };
        };
        responses: {
            /** @description Successful JSON response. Baseline schema is intentionally loose until per-route response DTO contracts are stabilized. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["JsonValue"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    taskInput: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Agent id. */
                agent_id: string;
                /** @description Task id. */
                task_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["TaskInputRequest"];
            };
        };
        responses: {
            /** @description Successful JSON response using a stable DTO schema. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["TaskInputResult"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    taskStop: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Agent id. */
                agent_id: string;
                /** @description Task id. */
                task_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["TaskStopRequest"];
            };
        };
        responses: {
            /** @description Successful JSON response using a stable DTO schema. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["TaskStopResult"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    createTimer: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Agent id. */
                agent_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CreateTimerRequest"];
            };
        };
        responses: {
            /** @description Successful JSON response using a stable DTO schema. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["TimerRecord"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    cancelTimer: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Agent id. */
                agent_id: string;
                /** @description Timer id. */
                timer_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CancelTimerRequest"];
            };
        };
        responses: {
            /** @description Successful JSON response using a stable DTO schema. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["TimerRecord"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    controlWake: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Agent id. */
                agent_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["ControlWakeRequest"];
            };
        };
        responses: {
            /** @description Successful JSON response. Baseline schema is intentionally loose until per-route response DTO contracts are stabilized. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["JsonValue"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    createWorkItem: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Agent id. */
                agent_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CreateWorkItemRequest"];
            };
        };
        responses: {
            /** @description Successful JSON response using a stable DTO schema. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["WorkItemRecord"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    updateWorkItem: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Agent id. */
                agent_id: string;
                /** @description Work item id. */
                work_item_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["UpdateWorkItemRequest"];
            };
        };
        responses: {
            /** @description Successful JSON response using a stable DTO schema. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["WorkItemRecord"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    completeWorkItem: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Agent id. */
                agent_id: string;
                /** @description Work item id. */
                work_item_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CompleteWorkItemRequest"];
            };
        };
        responses: {
            /** @description Successful JSON response using a stable DTO schema. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["WorkItemRecord"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    pickWorkItem: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Agent id. */
                agent_id: string;
                /** @description Work item id. */
                work_item_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["PickWorkItemRequest"];
            };
        };
        responses: {
            /** @description Successful JSON response using a stable DTO schema. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["PickWorkItemResponse"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    attachWorkspace: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Agent id. */
                agent_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["AttachWorkspaceRequest"];
            };
        };
        responses: {
            /** @description Successful JSON response. Baseline schema is intentionally loose until per-route response DTO contracts are stabilized. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["JsonValue"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    detachWorkspace: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Agent id. */
                agent_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["DetachWorkspaceRequest"];
            };
        };
        responses: {
            /** @description Successful JSON response. Baseline schema is intentionally loose until per-route response DTO contracts are stabilized. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["JsonValue"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    exitWorkspace: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Agent id. */
                agent_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["ExitWorkspaceRequest"];
            };
        };
        responses: {
            /** @description Successful JSON response. Baseline schema is intentionally loose until per-route response DTO contracts are stabilized. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["JsonValue"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    runtimeConfig: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Successful JSON response using a stable DTO schema. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["RuntimeConfigReadResponse"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    runtimeConfigUpdate: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["RuntimeConfigUpdateRequest"];
            };
        };
        responses: {
            /** @description Successful JSON response using a stable DTO schema. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["RuntimeConfigUpdateResponse"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    migrateModelConfigRoutes: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["ModelConfigMigrationRequest"];
            };
        };
        responses: {
            /** @description Successful JSON response using a stable DTO schema. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ModelConfigMigrationReport"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    runtimeCredentials: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Successful JSON response. Baseline schema is intentionally loose until per-route response DTO contracts are stabilized. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["JsonValue"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    setRuntimeCredential: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["SetCredentialRequest"];
            };
        };
        responses: {
            /** @description Successful JSON response. Baseline schema is intentionally loose until per-route response DTO contracts are stabilized. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["JsonValue"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    deleteRuntimeCredential: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Successful JSON response. Baseline schema is intentionally loose until per-route response DTO contracts are stabilized. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["JsonValue"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    runtimePerformance: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Successful JSON response using a stable DTO schema. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["PerformanceDiagnosticsSnapshot"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    runtimeReadiness: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Successful JSON response. Baseline schema is intentionally loose until per-route response DTO contracts are stabilized. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["JsonValue"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    runtimeShutdown: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Successful JSON response. Baseline schema is intentionally loose until per-route response DTO contracts are stabilized. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["JsonValue"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    runtimeStatus: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Successful JSON response. Baseline schema is intentionally loose until per-route response DTO contracts are stabilized. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["JsonValue"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    installTemplate: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["InstallTemplateRequest"];
            };
        };
        responses: {
            /** @description Successful JSON response. Baseline schema is intentionally loose until per-route response DTO contracts are stabilized. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["JsonValue"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    removeTemplate: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["RemoveTemplateRequest"];
            };
        };
        responses: {
            /** @description Successful JSON response. Baseline schema is intentionally loose until per-route response DTO contracts are stabilized. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["JsonValue"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    enqueueDefault: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["EnqueueRequest"];
            };
        };
        responses: {
            /** @description Successful JSON response. Baseline schema is intentionally loose until per-route response DTO contracts are stabilized. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["JsonValue"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    eventsStream: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Server-Sent Events stream. Each data frame contains a StreamEventEnvelope JSON object. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "text/event-stream": string;
                };
            };
            /** @description Client error before stream establishment. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    handshake: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Successful JSON response. Baseline schema is intentionally loose until per-route response DTO contracts are stabilized. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["JsonValue"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    createJob: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CreateJobRequest"];
            };
        };
        responses: {
            /** @description Successful JSON response using a stable DTO schema. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["JobResponse"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    jobStatus: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Successful JSON response using a stable DTO schema. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["JobResponse"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    runtimeMemoryGet: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["MemoryGetRequest"];
            };
        };
        responses: {
            /** @description Successful JSON response using a stable DTO schema. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["MemoryGetResult"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    models: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Successful JSON response. Baseline schema is intentionally loose until per-route response DTO contracts are stabilized. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["JsonValue"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    runtimeSearch: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["SearchRequest"];
            };
        };
        responses: {
            /** @description Successful JSON response using a stable DTO schema. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["SearchResponse"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    skillsCatalog: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Successful JSON response. Baseline schema is intentionally loose until per-route response DTO contracts are stabilized. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["JsonValue"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    addSkillToCatalog: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["AddSkillRequest"];
            };
        };
        responses: {
            /** @description Successful JSON response. Baseline schema is intentionally loose until per-route response DTO contracts are stabilized. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["JsonValue"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    checkSkillCatalog: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CheckSkillRequest"];
            };
        };
        responses: {
            /** @description Successful JSON response. Baseline schema is intentionally loose until per-route response DTO contracts are stabilized. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["JsonValue"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    reconcileSkillCatalog: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["ReconcileSkillRequest"];
            };
        };
        responses: {
            /** @description Successful JSON response. Baseline schema is intentionally loose until per-route response DTO contracts are stabilized. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["JsonValue"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    refreshSkillCatalog: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["RefreshCatalogRequest"];
            };
        };
        responses: {
            /** @description Successful JSON response. Baseline schema is intentionally loose until per-route response DTO contracts are stabilized. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["JsonValue"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    removeSkillFromCatalog: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["RemoveSkillRequest"];
            };
        };
        responses: {
            /** @description Successful JSON response. Baseline schema is intentionally loose until per-route response DTO contracts are stabilized. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["JsonValue"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    updateSkillCatalog: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["UpdateSkillRequest"];
            };
        };
        responses: {
            /** @description Successful JSON response using a stable DTO schema. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["JobResponse"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    skillDetail: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Root-qualified skill id. */
                skill_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Successful JSON response. Baseline schema is intentionally loose until per-route response DTO contracts are stabilized. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["JsonValue"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    defaultState: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Successful JSON response using a stable DTO schema. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["AgentStateSnapshotDto"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    defaultStatus: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Successful JSON response. Baseline schema is intentionally loose until per-route response DTO contracts are stabilized. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["JsonValue"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    templatesCatalog: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Successful JSON response. Baseline schema is intentionally loose until per-route response DTO contracts are stabilized. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["JsonValue"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    checkTemplate: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CheckTemplateRequest"];
            };
        };
        responses: {
            /** @description Successful JSON response. Baseline schema is intentionally loose until per-route response DTO contracts are stabilized. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["JsonValue"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    templateDetail: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Successful JSON response. Baseline schema is intentionally loose until per-route response DTO contracts are stabilized. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["JsonValue"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    syncTemplateRemoteSources: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["SyncTemplateRemoteSourcesRequest"];
            };
        };
        responses: {
            /** @description Successful JSON response. Baseline schema is intentionally loose until per-route response DTO contracts are stabilized. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["JsonValue"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    defaultTranscript: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Successful JSON response. Baseline schema is intentionally loose until per-route response DTO contracts are stabilized. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["JsonValue"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    genericWebhook: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Agent id. */
                agent_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["GenericJsonPayload"];
            };
        };
        responses: {
            /** @description Successful JSON response. Baseline schema is intentionally loose until per-route response DTO contracts are stabilized. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["JsonValue"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    workspaceFilesRoot: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Successful JSON response. Baseline schema is intentionally loose until per-route response DTO contracts are stabilized. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["JsonValue"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    workspaceFiles: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Successful JSON response. Baseline schema is intentionally loose until per-route response DTO contracts are stabilized. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["JsonValue"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
    defaultWorktreeSummary: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Successful JSON response. Baseline schema is intentionally loose until per-route response DTO contracts are stabilized. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["JsonValue"];
                };
            };
            /** @description Client error JSON response. */
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
            /** @description Server error JSON response. */
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorResponse"];
                };
            };
        };
    };
}
