package serve

import (
	"bufio"
	"context"
	"fmt"
	"io"
	"net"
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
	started   bool
	stopped   bool
	process   *os.Process
	startTime time.Time
}

// ForwarderConfig holds configuration for gh webhook forward
type ForwarderConfig struct {
	Port  int
	Repos []string
	URL   string // e.g., "http://127.0.0.1:8080/ingress/github/webhook"
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
		port: cfg.Port,
		repos: cfg.Repos,
		url:   cfg.URL,
	}, nil
}

// Start starts the gh webhook forward subprocess
func (f *Forwarder) Start(ctx context.Context) error {
	f.mu.Lock()
	defer f.mu.Unlock()

	if f.started {
		return fmt.Errorf("forwarder already started")
	}
	if f.stopped {
		return fmt.Errorf("forwarder was stopped and cannot be restarted")
	}

	// Build gh webhook forward command
	args := []string{"webhook", "forward"}
	for _, repo := range f.repos {
		args = append(args, "--repo="+repo)
	}
	args = append(args,
		"--events=issues,issue_comment,pull_request,pull_request_review,pull_request_review_comment",
		"--url="+f.url,
	)

	ctx, cancel := context.WithCancel(ctx)
	f.cancel = cancel

	f.cmd = exec.CommandContext(ctx, "gh", args...)
	f.cmd.SysProcAttr = &syscall.SysProcAttr{
		Setpgid: true,
	}

	// Capture stdout and stderr for logging
	stdout, err := f.cmd.StdoutPipe()
	if err != nil {
		cancel()
		return fmt.Errorf("failed to create stdout pipe: %w", err)
	}
	stderr, err := f.cmd.StderrPipe()
	if err != nil {
		cancel()
		return fmt.Errorf("failed to create stderr pipe: %w", err)
	}

	// Start the command
	if err := f.cmd.Start(); err != nil {
		cancel()
		return fmt.Errorf("failed to start gh webhook forward: %w", err)
	}

	f.process = f.cmd.Process
	f.started = true
	f.startTime = time.Now()

	// Start goroutines to log output
	go f.logOutput(stdout, "gh webhook forward (stdout)")
	go f.logOutput(stderr, "gh webhook forward (stderr)")

	// Start goroutine to wait for command completion
	go func() {
		err := f.cmd.Wait()
		f.mu.Lock()
		if f.started && !f.stopped {
			holonlog.Warn("gh webhook forward process exited unexpectedly", "error", err)
			f.started = false
		}
		f.mu.Unlock()
	}()

	holonlog.Info(
		"gh webhook forward started",
		"pid", f.process.Pid,
		"port", f.port,
		"repos", strings.Join(f.repos, ","),
		"url", f.url,
	)

	return nil
}

func (f *Forwarder) logOutput(r io.Reader, prefix string) {
	scanner := bufio.NewScanner(r)
	for scanner.Scan() {
		line := scanner.Text()
		holonlog.Debug(prefix, "line", line)
	}
	if err := scanner.Err(); err != nil {
		holonlog.Warn(prefix+" read error", "error", err)
	}
}

// Stop stops the gh webhook forward subprocess
func (f *Forwarder) Stop() error {
	f.mu.Lock()
	defer f.mu.Unlock()

	if !f.started {
		return nil
	}

	f.stopped = true

	if f.cancel != nil {
		f.cancel()
	}

	// Send SIGTERM to the process group
	if f.process != nil {
		holonlog.Info("stopping gh webhook forward", "pid", f.process.Pid)

		// Try graceful shutdown first
		if err := f.process.Signal(syscall.SIGTERM); err != nil {
			holonlog.Warn("failed to send SIGTERM to gh webhook forward", "error", err)
		}

		// Wait up to 5 seconds for graceful shutdown
		done := make(chan error, 1)
		go func() {
			_, err := f.process.Wait()
			done <- err
		}()

		select {
		case <-done:
			holonlog.Info("gh webhook forward stopped gracefully")
		case <-time.After(5 * time.Second):
			holonlog.Warn("gh webhook forward did not stop gracefully, forcing")
			if err := f.process.Kill(); err != nil {
				holonlog.Warn("failed to kill gh webhook forward", "error", err)
			}
		}
	}

	f.started = false
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

	if f.process == nil {
		return fmt.Errorf("forwarder process is nil")
	}

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
