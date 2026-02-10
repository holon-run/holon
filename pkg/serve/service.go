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
	"sync"
	"sync/atomic"
	"time"
)

type EventHandler interface {
	HandleEvent(ctx context.Context, env EventEnvelope) error
}

// SkipEventError indicates the event is valid but should not trigger execution.
type SkipEventError struct {
	Reason string
}

func (e *SkipEventError) Error() string {
	if e.Reason == "" {
		return "event skipped"
	}
	return e.Reason
}

func NewSkipEventError(reason string) error {
	return &SkipEventError{Reason: reason}
}

func IsSkipEventError(err error) bool {
	var skipErr *SkipEventError
	return errors.As(err, &skipErr)
}

type Service struct {
	handler    EventHandler
	repoHint   string
	statePath  string
	eventsLog  *ndjsonWriter
	decLog     *ndjsonWriter
	actionsLog *ndjsonWriter
	state      persistentState
	now        func() time.Time
	mu         sync.Mutex
}

var idCounter uint64

type Config struct {
	RepoHint string
	StateDir string
	Handler  EventHandler
}

type persistentState struct {
	LastEventID  string            `json:"last_event_id,omitempty"`
	ProcessedAt  map[string]string `json:"processed_at,omitempty"`
	ProcessedMax int               `json:"processed_max,omitempty"`
}

func New(cfg Config) (*Service, error) {
	if cfg.Handler == nil {
		return nil, errors.New("event handler is required")
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
		handler:    cfg.Handler,
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
	// GitHub payload lines can exceed Scanner's default 64 KiB token limit.
	scanner.Buffer(make([]byte, 0, 128*1024), 10*1024*1024)
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
		if err := s.processEnvelope(ctx, env); err != nil {
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

func (s *Service) InjectEvent(ctx context.Context, env EventEnvelope) error {
	if env.ID == "" {
		env.ID = newID("evt", s.now().UTC())
	}
	if env.At.IsZero() {
		env.At = s.now().UTC()
	}
	if env.Scope.Repo == "" {
		env.Scope.Repo = s.repoHint
	}
	if env.DedupeKey == "" {
		env.DedupeKey = buildDedupeKey(env)
	}
	return s.processEnvelope(ctx, env)
}

func (s *Service) processEnvelope(ctx context.Context, env EventEnvelope) error {
	s.mu.Lock()
	defer s.mu.Unlock()

	if err := s.eventsLog.Write(env); err != nil {
		return err
	}

	decision := DecisionRecord{
		ID:        newID("decision", s.now().UTC()),
		EventID:   env.ID,
		Type:      "forward_event",
		CreatedAt: s.now().UTC(),
	}
	if env.DedupeKey != "" {
		if _, exists := s.state.ProcessedAt[env.DedupeKey]; exists {
			decision.Skipped = true
			decision.Reason = "duplicate dedupe_key"
			if err := s.decLog.Write(decision); err != nil {
				return err
			}
			// Even for duplicate events, advance cursor state to avoid replay loops.
			s.state.LastEventID = env.ID
			if err := s.compactState(); err != nil {
				return err
			}
			if err := s.saveState(); err != nil {
				return err
			}
			return nil
		}
	}
	if err := s.decLog.Write(decision); err != nil {
		return err
	}

	start := s.now().UTC()
	result := ActionResult{
		ID:        newID("actres", start),
		EventID:   env.ID,
		StartedAt: start,
		EndedAt:   start,
	}

	if err := s.handler.HandleEvent(ctx, env); err != nil {
		if IsSkipEventError(err) {
			result.Status = "skipped"
			result.Message = err.Error()
		} else {
			result.Status = "failed"
			result.Message = err.Error()
		}
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
	return nil
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
		eventName = getHeaderValue(payload, "x-github-event")
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
		subjectKind := "issue"
		if _, hasPR := nestedMap(payload, "issue", "pull_request"); hasPR {
			subjectKind = "pull_request"
		}
		event.Subject = EventSubject{Kind: subjectKind, ID: strconv.Itoa(num)}
		event.Type = "github." + subjectKind + "." + strings.ToLower(action)
	case "issue_comment":
		num, _ := nestedInt(payload, "issue", "number")
		normalizedAction := strings.ToLower(action)
		switch normalizedAction {
		case "created", "edited", "deleted":
		default:
			return EventEnvelope{}, fmt.Errorf("unsupported issue_comment action: %s", action)
		}
		if _, hasPR := nestedMap(payload, "issue", "pull_request"); hasPR {
			event.Subject = EventSubject{Kind: "pull_request", ID: strconv.Itoa(num)}
			event.Type = "github.pull_request.comment." + normalizedAction
		} else {
			event.Subject = EventSubject{Kind: "issue", ID: strconv.Itoa(num)}
			event.Type = "github.issue.comment." + normalizedAction
		}
	case "pull_request":
		num := getPRNumber(payload)
		event.Subject = EventSubject{Kind: "pull_request", ID: strconv.Itoa(num)}
		event.Type = "github.pull_request." + strings.ToLower(action)
	case "pull_request_review_comment":
		num := getPRNumber(payload)
		event.Subject = EventSubject{Kind: "pull_request", ID: strconv.Itoa(num)}
		normalizedAction := strings.ToLower(action)
		switch normalizedAction {
		case "created", "edited", "deleted":
			event.Type = "github.pull_request_review_comment." + normalizedAction
		default:
			return EventEnvelope{}, fmt.Errorf("unsupported pull_request_review_comment action: %s", action)
		}
	case "pull_request_review":
		num := getPRNumber(payload)
		event.Subject = EventSubject{Kind: "pull_request", ID: strconv.Itoa(num)}
		normalizedAction := strings.ToLower(action)
		switch normalizedAction {
		case "submitted", "edited", "dismissed":
			event.Type = "github.pull_request_review." + normalizedAction
		default:
			return EventEnvelope{}, fmt.Errorf("unsupported pull_request_review action: %s", action)
		}
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
	event.DedupeKey = buildGitHubDedupeKey(event, payload)
	return event, nil
}

func buildDedupeKey(env EventEnvelope) string {
	if env.Type == "" {
		return ""
	}
	return strings.Trim(strings.Join([]string{env.Source, env.Scope.Repo, env.Subject.Kind, env.Subject.ID, env.Type}, ":"), ":")
}

func buildGitHubDedupeKey(env EventEnvelope, payload map[string]interface{}) string {
	if deliveryID, ok := getString(payload, "x_github_delivery"); ok && deliveryID != "" {
		return "github:delivery:" + deliveryID
	}
	if deliveryID := getHeaderValue(payload, "x-github-delivery"); deliveryID != "" {
		return "github:delivery:" + deliveryID
	}

	// Some GitHub events are emitted from multiple sources (issues + pull_request)
	// for the same user action. Normalize label-change keys to avoid double work.
	if labelAction, ok := isLabelAction(env.Type); ok {
		labelName, _ := nestedString(payload, "label", "name")
		return strings.Trim(strings.Join([]string{
			"github",
			env.Scope.Repo,
			env.Subject.Kind,
			env.Subject.ID,
			"label",
			labelAction,
			strings.ToLower(labelName),
		}, ":"), ":")
	}

	switch env.Type {
	case "github.issue.comment.created", "github.issue.comment.edited", "github.issue.comment.deleted":
		if id, ok := nestedInt(payload, "comment", "id"); ok {
			return strings.Trim(strings.Join([]string{
				"github", env.Scope.Repo, env.Subject.Kind, env.Subject.ID, "issue_comment", strconv.Itoa(id), actionFromType(env.Type),
			}, ":"), ":")
		}
	case "github.pull_request.comment.created", "github.pull_request.comment.edited", "github.pull_request.comment.deleted":
		if id, ok := nestedInt(payload, "comment", "id"); ok {
			return strings.Trim(strings.Join([]string{
				"github", env.Scope.Repo, "pull_request", env.Subject.ID, "comment", strconv.Itoa(id), actionFromType(env.Type),
			}, ":"), ":")
		}
	case "github.pull_request_review_comment.created", "github.pull_request_review_comment.edited", "github.pull_request_review_comment.deleted":
		if id, ok := nestedInt(payload, "comment", "id"); ok {
			return strings.Trim(strings.Join([]string{
				"github", env.Scope.Repo, "pull_request", env.Subject.ID, "review_comment", strconv.Itoa(id), actionFromType(env.Type),
			}, ":"), ":")
		}
	case "github.pull_request_review.submitted", "github.pull_request_review.edited", "github.pull_request_review.dismissed":
		if id, ok := nestedInt(payload, "review", "id"); ok {
			return strings.Trim(strings.Join([]string{
				"github", env.Scope.Repo, "pull_request", env.Subject.ID, "review", strconv.Itoa(id), actionFromType(env.Type),
			}, ":"), ":")
		}
	}

	return buildDedupeKey(env)
}

func actionFromType(eventType string) string {
	if idx := strings.LastIndex(eventType, "."); idx >= 0 && idx < len(eventType)-1 {
		return eventType[idx+1:]
	}
	return eventType
}

func isLabelAction(eventType string) (string, bool) {
	switch {
	case strings.HasSuffix(eventType, ".labeled"):
		return "labeled", true
	case strings.HasSuffix(eventType, ".unlabeled"):
		return "unlabeled", true
	default:
		return "", false
	}
}

func newID(prefix string, t time.Time) string {
	seq := atomic.AddUint64(&idCounter, 1)
	return fmt.Sprintf("%s_%d_%d", prefix, t.UnixNano(), seq)
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

func getHeaderValue(payload map[string]interface{}, key string) string {
	headers, ok := payload["headers"].(map[string]interface{})
	if !ok {
		return ""
	}
	for k, v := range headers {
		if !strings.EqualFold(k, key) {
			continue
		}
		switch val := v.(type) {
		case string:
			return val
		case []interface{}:
			if len(val) == 0 {
				return ""
			}
			if first, ok := val[0].(string); ok {
				return first
			}
		}
	}
	return ""
}
