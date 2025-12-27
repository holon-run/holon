package log

import (
	"testing"
)

func TestMapLevelToZapLevel(t *testing.T) {
	tests := []struct {
		name     string
		level    LogLevel
		expected string
	}{
		{"debug level", LevelDebug, "debug"},
		{"info level", LevelInfo, "info"},
		{"progress level", LevelProgress, "info"},
		{"minimal level", LevelMinimal, "warn"},
		{"warn level", LevelWarn, "warn"},
		{"error level", LevelError, "error"},
		{"unknown level defaults to info", LogLevel("unknown"), "info"},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			zapLevel, _ := mapLevelToZapLevel(tt.level)
			if zapLevel.String() != tt.expected {
				t.Errorf("mapLevelToZapLevel() = %v, want %v", zapLevel.String(), tt.expected)
			}
		})
	}
}

func TestInitWithConfig(t *testing.T) {
	// Reset before test
	Reset()
	defer Reset()

	// Test initialization with different levels
	levels := []LogLevel{
		LevelDebug,
		LevelInfo,
		LevelProgress,
		LevelMinimal,
		LevelWarn,
		LevelError,
	}

	for _, level := range levels {
		t.Run(string(level), func(t *testing.T) {
			Reset()
			cfg := Config{
				Level:  level,
				Format: "console",
			}
			if err := Init(cfg); err != nil {
				t.Errorf("Init() error = %v", err)
			}

			logger := Get()
			if logger == nil {
				t.Error("Get() returned nil logger")
			}
		})
	}
}

func TestLogLevels(t *testing.T) {
	// Capture output for testing
	Reset()
	defer Reset()

	cfg := Config{
		Level:  LevelDebug,
		Format: "console",
	}
	if err := Init(cfg); err != nil {
		t.Fatalf("Init() error = %v", err)
	}

	// Test various log methods - these should not panic
	tests := []struct {
		name string
		fn   func()
	}{
		{"Debug", func() { Debug("test debug message") }},
		{"Debugf", func() { Debugf("test debug %s", "formatted") }},
		{"Info", func() { Info("test info message") }},
		{"Infof", func() { Infof("test info %s", "formatted") }},
		{"Progress", func() { Progress("test progress message") }},
		{"Progressf", func() { Progressf("test progress %s", "formatted") }},
		{"Warn", func() { Warn("test warn message") }},
		{"Warnf", func() { Warnf("test warn %s", "formatted") }},
		{"Error", func() { Error("test error message") }},
		{"Errorf", func() { Errorf("test error %s", "formatted") }},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			// Just verify the calls don't panic - output capture is complex with zap
			tt.fn()
		})
	}
}

func TestDefaultConfig(t *testing.T) {
	cfg := DefaultConfig()
	if cfg.Level != LevelProgress {
		t.Errorf("DefaultConfig().Level = %v, want %v", cfg.Level, LevelProgress)
	}
	if cfg.Format != "console" {
		t.Errorf("DefaultConfig().Format = %v, want %v", cfg.Format, "console")
	}
}

func TestGetInitializesDefaultLogger(t *testing.T) {
	Reset()
	defer Reset()

	// Get should initialize with default config if not initialized
	logger := Get()
	if logger == nil {
		t.Error("Get() returned nil logger")
	}

	// Verify default config is used
	if logger != Get() {
		t.Error("Get() returned different logger instances")
	}
}

func TestWith(t *testing.T) {
	Reset()
	defer Reset()

	cfg := Config{
		Level:  LevelDebug,
		Format: "console",
	}
	if err := Init(cfg); err != nil {
		t.Fatalf("Init() error = %v", err)
	}

	logger := With("key", "value")
	if logger == nil {
		t.Error("With() returned nil logger")
	}
}

func TestSync(t *testing.T) {
	Reset()
	defer Reset()

	cfg := Config{
		Level:  LevelDebug,
		Format: "console",
	}
	if err := Init(cfg); err != nil {
		t.Fatalf("Init() error = %v", err)
	}

	// Sync may fail in some environments (e.g., when syncing stdout)
	// The important thing is that it doesn't panic
	_ = Sync()
}
