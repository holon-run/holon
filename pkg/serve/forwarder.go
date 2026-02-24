package serve

import (
	"bufio"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net"
	neturl "net/url"
	"os"
	"os/exec"
	"strconv"
	"strings"
	"sync"
	"syscall"
	"time"

	holonlog "github.com/holon-run/holon/pkg/log"
)

// Forwarder manages gh webhook forward subprocess lifecycle
type Forwarder struct {
	cmd       *exec.Cmd
	port      int
	repos     []string
	url       string
	cancel    context.CancelFunc
	mu        sync.Mutex
	starting  bool
	started   bool
	stopped   bool
	process   *os.Process
	startTime time.Time
	waitDone  chan struct{}
	waitErr   error
	stderrLog []string
}

// ForwarderConfig holds configuration for gh webhook forward
type ForwarderConfig struct {
	Port  int
	Repos []string
	URL   string // e.g., "http://127.0.0.1:8080/ingress/github/webhook"
}

const (
	webhookEvents                 = "issues,issue_comment,pull_request,pull_request_review,pull_request_review_comment"
	forwarderStartupGracePeriod   = 1200 * time.Millisecond
	forwarderStderrCaptureLineMax = 64
	webhookTargetFlagURL          = "url"
	webhookTargetFlagPort         = "port"
	existingHookConflictMarker    = "Hook already exists on this repository"
)

type githubRepoHook struct {
	ID     int64    `json:"id"`
	Events []string `json:"events"`
	Config struct {
		URL string `json:"url"`
	} `json:"config"`
}

var listGitHubRepoHooks = func(ctx context.Context, repo string) ([]githubRepoHook, error) {
	cmd := exec.CommandContext(ctx, "gh", "api", fmt.Sprintf("repos/%s/hooks", repo))
	out, err := cmd.Output()
	if err != nil {
		if ee, ok := err.(*exec.ExitError); ok {
			return nil, fmt.Errorf("gh api repos/%s/hooks failed: %w (stderr: %s)", repo, err, strings.TrimSpace(string(ee.Stderr)))
		}
		return nil, fmt.Errorf("gh api repos/%s/hooks failed: %w", repo, err)
	}
	var hooks []githubRepoHook
	if err := json.Unmarshal(out, &hooks); err != nil {
		return nil, fmt.Errorf("decode repo hooks for %s: %w", repo, err)
	}
	return hooks, nil
}

// NewForwarder creates a new gh webhook forward manager
func NewForwarder(cfg ForwarderConfig) (*Forwarder, error) {
	if cfg.Port <= 0 {
		return nil, fmt.Errorf("invalid port: %d", cfg.Port)
	}
	if len(cfg.Repos) == 0 {
		return nil, fmt.Errorf("at least one repo is required")
	}
	if cfg.URL == "" {
		return nil, fmt.Errorf("url is required")
	}

	// Validate repo format
	for _, repo := range cfg.Repos {
		parts := strings.Split(repo, "/")
		if len(parts) != 2 {
			return nil, fmt.Errorf("invalid repo format %q (expected owner/repo)", repo)
		}
		if strings.TrimSpace(parts[0]) == "" || strings.TrimSpace(parts[1]) == "" {
			return nil, fmt.Errorf("invalid repo format %q (expected owner/repo)", repo)
		}
	}

	return &Forwarder{
		port:  cfg.Port,
		repos: cfg.Repos,
		url:   cfg.URL,
	}, nil
}

// Start starts the gh webhook forward subprocess
func (f *Forwarder) Start(ctx context.Context) error {
	f.mu.Lock()
	if f.started {
		f.mu.Unlock()
		return fmt.Errorf("forwarder already started")
	}
	if f.starting {
		f.mu.Unlock()
		return fmt.Errorf("forwarder already starting")
	}
	if f.stopped {
		f.mu.Unlock()
		return fmt.Errorf("forwarder was stopped and cannot be restarted")
	}
	f.starting = true
	f.mu.Unlock()

	// Build gh webhook forward command
	targetFlag, err := detectWebhookForwardTargetFlag(ctx)
	if err != nil {
		f.clearStarting()
		return fmt.Errorf("failed to detect gh webhook target flag: %w", err)
	}
	args := []string{"webhook", "forward"}
	for _, repo := range f.repos {
		args = append(args, "--repo="+repo)
	}
	args = append(args, "--events="+webhookEvents)
	switch targetFlag {
	case webhookTargetFlagURL:
		args = append(args, "--url="+f.url)
	case webhookTargetFlagPort:
		args = append(args, "--port="+strconv.Itoa(f.port))
	default:
		f.clearStarting()
		return fmt.Errorf("unsupported webhook target flag: %s", targetFlag)
	}

	cmdCtx, cancel := context.WithCancel(ctx)

	cmd := exec.CommandContext(cmdCtx, "gh", args...)
	cmd.SysProcAttr = &syscall.SysProcAttr{
		Setpgid: true,
	}

	// Capture stdout and stderr for logging
	stdout, err := cmd.StdoutPipe()
	if err != nil {
		cancel()
		f.clearStarting()
		return fmt.Errorf("failed to create stdout pipe: %w", err)
	}
	stderr, err := cmd.StderrPipe()
	if err != nil {
		cancel()
		// Clean up stdout pipe
		_ = stdout.Close()
		f.clearStarting()
		return fmt.Errorf("failed to create stderr pipe: %w", err)
	}

	// Start the command
	if err := cmd.Start(); err != nil {
		cancel()
		// Clean up pipes
		_ = stdout.Close()
		_ = stderr.Close()
		f.clearStarting()
		return fmt.Errorf("failed to start gh webhook forward: %w", err)
	}

	f.mu.Lock()
	f.cancel = cancel
	f.cmd = cmd
	f.process = cmd.Process
	f.starting = false
	f.started = true
	f.stopped = false
	f.startTime = time.Now()
	f.waitDone = make(chan struct{})
	f.waitErr = nil
	f.stderrLog = nil
	waitDone := f.waitDone
	f.mu.Unlock()

	// Start goroutines to log output
	go f.logOutput(stdout, "gh webhook forward (stdout)", false)
	go f.logOutput(stderr, "gh webhook forward (stderr)", true)

	// Start goroutine to wait for command completion
	go func() {
		err := cmd.Wait()
		f.mu.Lock()
		f.waitErr = err
		waitDone := f.waitDone
		stderrTail := strings.Join(f.stderrLog, " | ")
		if f.started && !f.stopped {
			if stderrTail != "" {
				holonlog.Warn("gh webhook forward process exited unexpectedly", "error", err, "stderr_tail", stderrTail)
			} else {
				holonlog.Warn("gh webhook forward process exited unexpectedly", "error", err)
			}
			f.started = false
		}
		f.mu.Unlock()
		if waitDone != nil {
			close(waitDone)
		}
	}()

	holonlog.Info(
		"gh webhook forward started",
		"pid", f.process.Pid,
		"port", f.port,
		"target_flag", targetFlag,
		"repos", strings.Join(f.repos, ","),
		"url", f.url,
	)

	select {
	case <-waitDone:
		f.mu.Lock()
		waitErr := f.waitErr
		stderrTail := strings.Join(f.stderrLog, " | ")
		f.mu.Unlock()
		return f.buildStartupFailureError(ctx, waitErr, stderrTail)
	case <-time.After(forwarderStartupGracePeriod):
	}

	return nil
}

func (f *Forwarder) buildStartupFailureError(ctx context.Context, waitErr error, stderrTail string) error {
	var baseErr error
	if waitErr != nil {
		if stderrTail != "" {
			baseErr = fmt.Errorf("gh webhook forward exited during startup: %w (stderr: %s)", waitErr, stderrTail)
		} else {
			baseErr = fmt.Errorf("gh webhook forward exited during startup: %w", waitErr)
		}
	} else {
		baseErr = fmt.Errorf("gh webhook forward exited during startup")
	}
	if !strings.Contains(stderrTail, existingHookConflictMarker) {
		return baseErr
	}

	remediation, err := buildExistingHookRemediation(ctx, f.repos, f.url)
	if err != nil {
		return fmt.Errorf("%w; detected existing webhook conflict but failed to inspect hooks: %v", baseErr, err)
	}
	if remediation == "" {
		return fmt.Errorf("%w; detected existing webhook conflict, remove the existing webhook and retry", baseErr)
	}
	return fmt.Errorf("%w; %s", baseErr, remediation)
}

func buildExistingHookRemediation(ctx context.Context, repos []string, targetURL string) (string, error) {
	if len(repos) == 0 {
		return "", fmt.Errorf("no repositories configured")
	}

	inspectCtx, cancel := context.WithTimeout(ctx, 5*time.Second)
	defer cancel()

	hints := make([]string, 0)
	var inspectErrs []string
	for _, repo := range repos {
		hooks, err := listGitHubRepoHooks(inspectCtx, repo)
		if err != nil {
			inspectErrs = append(inspectErrs, err.Error())
			continue
		}
		for _, hook := range hooks {
			if webhookURLsEquivalent(hook.Config.URL, targetURL) {
				hints = append(hints, fmt.Sprintf("existing hook id %d on %s (delete with: gh api -X DELETE repos/%s/hooks/%d)", hook.ID, repo, repo, hook.ID))
			}
		}
	}

	if len(hints) > 0 {
		return strings.Join(hints, "; "), nil
	}
	if len(inspectErrs) > 0 {
		return "", fmt.Errorf("%s", strings.Join(inspectErrs, "; "))
	}
	return "", nil
}

func webhookURLsEquivalent(a, b string) bool {
	na := normalizeWebhookURL(a)
	nb := normalizeWebhookURL(b)
	if na == "" || nb == "" {
		return false
	}
	if na == nb {
		return true
	}

	ua, errA := neturl.Parse(na)
	ub, errB := neturl.Parse(nb)
	if errA != nil || errB != nil {
		return false
	}
	if !strings.EqualFold(ua.Scheme, ub.Scheme) {
		return false
	}
	if !strings.EqualFold(ua.Path, ub.Path) {
		return false
	}
	if normalizePort(ua) != normalizePort(ub) {
		return false
	}
	return isLocalHost(ua.Hostname()) && isLocalHost(ub.Hostname())
}

func normalizeWebhookURL(raw string) string {
	trimmed := strings.TrimSpace(raw)
	if trimmed == "" {
		return ""
	}
	return strings.TrimRight(trimmed, "/")
}

func normalizePort(u *neturl.URL) string {
	port := u.Port()
	if port != "" {
		return port
	}
	switch strings.ToLower(u.Scheme) {
	case "https":
		return "443"
	case "http":
		return "80"
	default:
		return ""
	}
}

func isLocalHost(host string) bool {
	h := strings.TrimSpace(strings.ToLower(host))
	return h == "127.0.0.1" || h == "localhost"
}

func (f *Forwarder) clearStarting() {
	f.mu.Lock()
	f.starting = false
	f.mu.Unlock()
}

func (f *Forwarder) logOutput(r io.Reader, prefix string, captureTail bool) {
	scanner := bufio.NewScanner(r)
	for scanner.Scan() {
		line := scanner.Text()
		if captureTail {
			f.mu.Lock()
			f.stderrLog = append(f.stderrLog, line)
			if len(f.stderrLog) > forwarderStderrCaptureLineMax {
				f.stderrLog = f.stderrLog[len(f.stderrLog)-forwarderStderrCaptureLineMax:]
			}
			f.mu.Unlock()
		}
		holonlog.Debug(prefix, "line", line)
	}
	if err := scanner.Err(); err != nil {
		holonlog.Warn(prefix+" read error", "error", err)
	}
}

func detectWebhookForwardTargetFlag(ctx context.Context) (string, error) {
	detectCtx, cancel := context.WithTimeout(ctx, 3*time.Second)
	defer cancel()

	cmd := exec.CommandContext(detectCtx, "gh", "webhook", "forward", "--help")
	out, err := cmd.CombinedOutput()
	if err != nil {
		return "", fmt.Errorf("failed to run gh webhook forward --help: %w (output: %s)", err, strings.TrimSpace(string(out)))
	}
	return selectWebhookForwardTargetFlag(string(out))
}

func selectWebhookForwardTargetFlag(helpOutput string) (string, error) {
	switch {
	case strings.Contains(helpOutput, "--url"):
		return webhookTargetFlagURL, nil
	case strings.Contains(helpOutput, "--port"):
		return webhookTargetFlagPort, nil
	default:
		return "", fmt.Errorf("gh webhook forward help output missing --url/--port")
	}
}

// Stop stops the gh webhook forward subprocess
func (f *Forwarder) Stop() error {
	f.mu.Lock()
	if !f.started {
		f.mu.Unlock()
		return nil
	}

	f.stopped = true
	pid := 0
	if f.process != nil {
		pid = f.process.Pid
	}
	waitDone := f.waitDone
	cancel := f.cancel
	f.mu.Unlock()

	if cancel != nil {
		cancel()
	}

	// Send SIGTERM to the process group
	if pid > 0 {
		holonlog.Info("stopping gh webhook forward", "pid", pid)

		// Try graceful shutdown first
		if err := signalProcessGroup(pid, syscall.SIGTERM); err != nil {
			holonlog.Warn("failed to send SIGTERM to gh webhook forward", "error", err)
		}
	}

	if waitDone != nil {
		select {
		case <-waitDone:
			holonlog.Info("gh webhook forward stopped gracefully")
		case <-time.After(5 * time.Second):
			holonlog.Warn("gh webhook forward did not stop gracefully, forcing")
			if pid > 0 {
				if err := signalProcessGroup(pid, syscall.SIGKILL); err != nil {
					holonlog.Warn("failed to kill gh webhook forward", "error", err)
				}
			}
			select {
			case <-waitDone:
			case <-time.After(2 * time.Second):
			}
		}
	}

	f.mu.Lock()
	f.started = false
	f.process = nil
	f.cmd = nil
	f.cancel = nil
	f.waitDone = nil
	f.mu.Unlock()
	return nil
}

func signalProcessGroup(pid int, signal syscall.Signal) error {
	if pid <= 0 {
		return fmt.Errorf("invalid pid: %d", pid)
	}
	if err := syscall.Kill(-pid, signal); err != nil {
		// If process already exited, treat as non-fatal for shutdown.
		if err == syscall.ESRCH {
			return nil
		}
		return err
	}
	return nil
}

// IsRunning returns true if the forwarder is currently running
func (f *Forwarder) IsRunning() bool {
	f.mu.Lock()
	defer f.mu.Unlock()
	return f.started
}

// Pid returns the process ID if running, 0 otherwise
func (f *Forwarder) Pid() int {
	f.mu.Lock()
	defer f.mu.Unlock()
	if f.process != nil {
		return f.process.Pid
	}
	return 0
}

// Uptime returns how long the forwarder has been running
func (f *Forwarder) Uptime() time.Duration {
	f.mu.Lock()
	defer f.mu.Unlock()
	if !f.started {
		return 0
	}
	return time.Since(f.startTime)
}

// HealthCheck checks if the forwarder process is still alive
func (f *Forwarder) HealthCheck() error {
	f.mu.Lock()
	defer f.mu.Unlock()

	if !f.started {
		return fmt.Errorf("forwarder not started")
	}

	// f.process is guaranteed to be non-nil when f.started is true
	// (it's set in Start() before f.started)

	// Check if process is still running
	err := f.process.Signal(syscall.Signal(0))
	if err != nil {
		return fmt.Errorf("forwarder process check failed: %w", err)
	}

	return nil
}

// Status returns status information about the forwarder
func (f *Forwarder) Status() map[string]interface{} {
	f.mu.Lock()
	defer f.mu.Unlock()

	status := map[string]interface{}{
		"running": f.started,
		"port":    f.port,
		"repos":   f.repos,
		"url":     f.url,
	}

	if f.started && f.process != nil {
		status["pid"] = f.process.Pid
		status["uptime"] = time.Since(f.startTime).String()
	}

	return status
}

// BuildWebhookURL constructs the webhook URL from port and path
func BuildWebhookURL(port int, path string) string {
	if path == "" {
		path = "/ingress/github/webhook"
	}
	if !strings.HasPrefix(path, "/") {
		path = "/" + path
	}
	return fmt.Sprintf("http://127.0.0.1:%d%s", port, path)
}

// GetAvailablePort finds an available port on localhost
func GetAvailablePort() (int, error) {
	// Try ports in a reasonable range
	for port := 8080; port <= 9080; port++ {
		l, err := (&net.ListenConfig{}).Listen(context.Background(), "tcp", "127.0.0.1:"+strconv.Itoa(port))
		if err == nil {
			l.Close()
			return port, nil
		}
	}
	return 0, fmt.Errorf("no available port found in range 8080-9080")
}
