package serve

import (
	"bufio"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
	"time"
)

type Executor interface {
	Execute(ctx context.Context, action ActionIntent) error
}

type Service struct {
	exec       Executor
	repoHint   string
	statePath  string
	eventsLog  *ndjsonWriter
	decLog     *ndjsonWriter
	actionsLog *ndjsonWriter
	state      persistentState
	now        func() time.Time
}

type Config struct {
	RepoHint string
	StateDir string
	Exec     Executor
}

type persistentState struct {
	LastEventID  string            `json:"last_event_id,omitempty"`
	ProcessedAt  map[string]string `json:"processed_at,omitempty"`
	ProcessedMax int               `json:"processed_max,omitempty"`
}

func New(cfg Config) (*Service, error) {
	if cfg.Exec == nil {
		return nil, errors.New("executor is required")
	}
	if cfg.StateDir == "" {
		return nil, errors.New("state dir is required")
	}
	if err := os.MkdirAll(cfg.StateDir, 0755); err != nil {
		return nil, fmt.Errorf("failed to create state dir: %w", err)
	}
	eventsLog, err := newNDJSONWriter(filepath.Join(cfg.StateDir, "events.ndjson"))
	if err != nil {
		return nil, err
	}
	decLog, err := newNDJSONWriter(filepath.Join(cfg.StateDir, "decisions.ndjson"))
	if err != nil {
		return nil, err
	}
	actionsLog, err := newNDJSONWriter(filepath.Join(cfg.StateDir, "actions.ndjson"))
	if err != nil {
		return nil, err
	}

	s := &Service{
		exec:       cfg.Exec,
		repoHint:   cfg.RepoHint,
		statePath:  filepath.Join(cfg.StateDir, "serve-state.json"),
		eventsLog:  eventsLog,
		decLog:     decLog,
		actionsLog: actionsLog,
		now:        time.Now,
	}
	if err := s.loadState(); err != nil {
		return nil, err
	}
	if s.state.ProcessedMax <= 0 {
		s.state.ProcessedMax = 2000
	}
	if s.state.ProcessedAt == nil {
		s.state.ProcessedAt = make(map[string]string)
	}
	return s, nil
}

func (s *Service) Close() error {
	var firstErr error
	for _, w := range []*ndjsonWriter{s.eventsLog, s.decLog, s.actionsLog} {
		if w == nil {
			continue
		}
		if err := w.Close(); err != nil && firstErr == nil {
			firstErr = err
		}
	}
	return firstErr
}

func (s *Service) Run(ctx context.Context, r io.Reader, maxEvents int) error {
	scanner := bufio.NewScanner(r)
	processed := 0
	for scanner.Scan() {
		select {
		case <-ctx.Done():
			return ctx.Err()
		default:
		}
		line := strings.TrimSpace(scanner.Text())
		if line == "" {
			continue
		}
		env, err := normalizeLine([]byte(line), s.repoHint, s.now)
		if err != nil {
			return fmt.Errorf("failed to normalize event line: %w", err)
		}
		if err := s.eventsLog.Write(env); err != nil {
			return err
		}

		intent := s.decide(env)
		if intent.Type == "skip" && intent.Skipped {
			if err := s.decLog.Write(intent); err != nil {
				return err
			}
			continue
		}
		if err := s.decLog.Write(intent); err != nil {
			return err
		}
		start := s.now().UTC()
		result := ActionResult{
			ID:        newID("actres", start),
			EventID:   env.ID,
			ActionID:  intent.ID,
			StartedAt: start,
			EndedAt:   start,
		}
		if intent.Type == "wait" {
			result.Status = "skipped"
			result.Message = "no-op decision"
		} else if err := s.exec.Execute(ctx, intent); err != nil {
			result.Status = "failed"
			result.Message = err.Error()
		} else {
			result.Status = "ok"
		}
		result.EndedAt = s.now().UTC()
		if err := s.actionsLog.Write(result); err != nil {
			return err
		}

		if env.DedupeKey != "" {
			s.state.ProcessedAt[env.DedupeKey] = result.EndedAt.Format(time.RFC3339Nano)
		}
		s.state.LastEventID = env.ID
		if err := s.compactState(); err != nil {
			return err
		}
		if err := s.saveState(); err != nil {
			return err
		}
		processed++
		if maxEvents > 0 && processed >= maxEvents {
			return nil
		}
	}
	if err := scanner.Err(); err != nil {
		return fmt.Errorf("failed to read events: %w", err)
	}
	return nil
}

func (s *Service) decide(env EventEnvelope) ActionIntent {
	action := ActionIntent{
		ID:     newID("decision", s.now().UTC()),
		Target: ActionTarget{Repo: env.Scope.Repo, Kind: env.Subject.Kind, ID: env.Subject.ID},
	}
	if env.DedupeKey != "" {
		if _, exists := s.state.ProcessedAt[env.DedupeKey]; exists {
			action.Type = "skip"
			action.Skipped = true
			action.Reason = "duplicate dedupe_key"
			return action
		}
	}

	switch env.Type {
	case "github.issue.opened", "github.issue.reopened", "github.issue.comment.created":
		action.Type = "run_issue_solve"
		action.Reason = "issue event requires implementation follow-up"
	case "github.pull_request.opened", "github.pull_request.reopened", "github.pull_request.synchronize":
		action.Type = "run_pr_review"
		action.Reason = "pr update should trigger review"
	case "github.pull_request_review.submitted", "github.pull_request.comment.created", "github.check_suite.completed":
		if shouldRunPRFix(env) {
			action.Type = "run_pr_fix"
			action.Reason = "review/check requested fixes"
		} else {
			action.Type = "wait"
			action.Reason = "event does not require action"
		}
	default:
		action.Type = "wait"
		action.Reason = "unsupported event type"
	}
	return action
}

func shouldRunPRFix(env EventEnvelope) bool {
	if len(env.Payload) == 0 {
		return false
	}
	var payload map[string]interface{}
	if err := json.Unmarshal(env.Payload, &payload); err != nil {
		return false
	}
	if state, _ := nestedString(payload, "review", "state"); strings.EqualFold(state, "changes_requested") {
		return true
	}
	if conclusion, _ := nestedString(payload, "check_suite", "conclusion"); strings.EqualFold(conclusion, "failure") {
		return true
	}
	body, _ := nestedString(payload, "comment", "body")
	return strings.Contains(strings.ToLower(body), "@holonbot")
}

func normalizeLine(line []byte, repoHint string, now func() time.Time) (EventEnvelope, error) {
	var env EventEnvelope
	if err := json.Unmarshal(line, &env); err == nil && env.Type != "" {
		if env.ID == "" {
			env.ID = newID("evt", now().UTC())
		}
		if env.At.IsZero() {
			env.At = now().UTC()
		}
		if env.Scope.Repo == "" {
			env.Scope.Repo = repoHint
		}
		if env.DedupeKey == "" {
			env.DedupeKey = buildDedupeKey(env)
		}
		return env, nil
	}
	return normalizeGitHubEvent(line, repoHint, now)
}

func normalizeGitHubEvent(raw []byte, repoHint string, now func() time.Time) (EventEnvelope, error) {
	var payload map[string]interface{}
	if err := json.Unmarshal(raw, &payload); err != nil {
		return EventEnvelope{}, fmt.Errorf("invalid json: %w", err)
	}

	eventName, _ := getString(payload, "event")
	if eventName == "" {
		eventName, _ = getString(payload, "x_github_event")
	}
	if eventName == "" {
		return EventEnvelope{}, errors.New("missing event or x_github_event")
	}

	repo := repoHint
	if r, ok := nestedString(payload, "repository", "full_name"); ok && r != "" {
		repo = r
	}
	action, _ := getString(payload, "action")
	event := EventEnvelope{
		ID:     newID("evt", now().UTC()),
		Source: "github",
		At:     now().UTC(),
		Scope: EventScope{
			Repo: repo,
		},
	}

	switch eventName {
	case "issues":
		num, _ := nestedInt(payload, "issue", "number")
		event.Subject = EventSubject{Kind: "issue", ID: strconv.Itoa(num)}
		event.Type = "github.issue." + strings.ToLower(action)
	case "issue_comment":
		num, _ := nestedInt(payload, "issue", "number")
		if _, hasPR := nestedMap(payload, "issue", "pull_request"); hasPR {
			event.Subject = EventSubject{Kind: "pull_request", ID: strconv.Itoa(num)}
			event.Type = "github.pull_request.comment.created"
		} else {
			event.Subject = EventSubject{Kind: "issue", ID: strconv.Itoa(num)}
			event.Type = "github.issue.comment.created"
		}
	case "pull_request":
		num := getPRNumber(payload)
		event.Subject = EventSubject{Kind: "pull_request", ID: strconv.Itoa(num)}
		event.Type = "github.pull_request." + strings.ToLower(action)
	case "pull_request_review":
		num := getPRNumber(payload)
		event.Subject = EventSubject{Kind: "pull_request", ID: strconv.Itoa(num)}
		event.Type = "github.pull_request_review.submitted"
	case "check_suite":
		prs, ok := payload["pull_requests"].([]interface{})
		if ok && len(prs) > 0 {
			if first, ok := prs[0].(map[string]interface{}); ok {
				if n, ok := getInt(first, "number"); ok {
					event.Subject = EventSubject{Kind: "pull_request", ID: strconv.Itoa(n)}
				}
			}
		}
		event.Type = "github.check_suite.completed"
	default:
		return EventEnvelope{}, fmt.Errorf("unsupported event type: %s", eventName)
	}
	if len(event.Payload) == 0 {
		event.Payload = json.RawMessage(raw)
	}
	event.DedupeKey = buildDedupeKey(event)
	return event, nil
}

func buildDedupeKey(env EventEnvelope) string {
	if env.Type == "" {
		return ""
	}
	return strings.Trim(strings.Join([]string{env.Source, env.Scope.Repo, env.Subject.Kind, env.Subject.ID, env.Type}, ":"), ":")
}

func newID(prefix string, t time.Time) string {
	return fmt.Sprintf("%s_%d", prefix, t.UnixNano())
}

func (s *Service) loadState() error {
	data, err := os.ReadFile(s.statePath)
	if errors.Is(err, os.ErrNotExist) {
		s.state = persistentState{ProcessedAt: make(map[string]string)}
		return nil
	}
	if err != nil {
		return fmt.Errorf("failed to read serve state: %w", err)
	}
	if err := json.Unmarshal(data, &s.state); err != nil {
		return fmt.Errorf("failed to parse serve state: %w", err)
	}
	if s.state.ProcessedAt == nil {
		s.state.ProcessedAt = make(map[string]string)
	}
	return nil
}

func (s *Service) saveState() error {
	data, err := json.MarshalIndent(s.state, "", "  ")
	if err != nil {
		return fmt.Errorf("failed to marshal serve state: %w", err)
	}
	if err := os.WriteFile(s.statePath, data, 0644); err != nil {
		return fmt.Errorf("failed to write serve state: %w", err)
	}
	return nil
}

func (s *Service) compactState() error {
	if len(s.state.ProcessedAt) <= s.state.ProcessedMax {
		return nil
	}
	items := make([]stateItem, 0, len(s.state.ProcessedAt))
	for k, v := range s.state.ProcessedAt {
		t, err := time.Parse(time.RFC3339Nano, v)
		if err != nil {
			t = time.Time{}
		}
		items = append(items, stateItem{key: k, at: t})
	}
	// Keep the newest N entries; old malformed timestamps are dropped first.
	sortByTimeDesc(items)
	for idx := s.state.ProcessedMax; idx < len(items); idx++ {
		delete(s.state.ProcessedAt, items[idx].key)
	}
	return nil
}

func sortByTimeDesc(items []stateItem) {
	sort.Slice(items, func(i, j int) bool {
		return items[i].at.After(items[j].at)
	})
}

type stateItem struct {
	key string
	at  time.Time
}

func getPRNumber(payload map[string]interface{}) int {
	if n, ok := getInt(payload, "number"); ok {
		return n
	}
	if n, ok := nestedInt(payload, "pull_request", "number"); ok {
		return n
	}
	return 0
}

func getString(m map[string]interface{}, key string) (string, bool) {
	raw, ok := m[key]
	if !ok || raw == nil {
		return "", false
	}
	s, ok := raw.(string)
	return s, ok
}

func getInt(m map[string]interface{}, key string) (int, bool) {
	raw, ok := m[key]
	if !ok || raw == nil {
		return 0, false
	}
	switch n := raw.(type) {
	case float64:
		return int(n), true
	case int:
		return n, true
	default:
		return 0, false
	}
}

func nestedString(m map[string]interface{}, path ...string) (string, bool) {
	cur := m
	for i := 0; i < len(path)-1; i++ {
		next, ok := cur[path[i]].(map[string]interface{})
		if !ok {
			return "", false
		}
		cur = next
	}
	return getString(cur, path[len(path)-1])
}

func nestedInt(m map[string]interface{}, path ...string) (int, bool) {
	cur := m
	for i := 0; i < len(path)-1; i++ {
		next, ok := cur[path[i]].(map[string]interface{})
		if !ok {
			return 0, false
		}
		cur = next
	}
	return getInt(cur, path[len(path)-1])
}

func nestedMap(m map[string]interface{}, path ...string) (map[string]interface{}, bool) {
	cur := m
	for i := 0; i < len(path)-1; i++ {
		next, ok := cur[path[i]].(map[string]interface{})
		if !ok {
			return nil, false
		}
		cur = next
	}
	next, ok := cur[path[len(path)-1]].(map[string]interface{})
	return next, ok
}
