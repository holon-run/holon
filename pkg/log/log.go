package log

import (
	"os"
	"sync"

	"go.uber.org/zap"
	"go.uber.org/zap/zapcore"
)

// LogLevel represents the verbosity of logging
type LogLevel string

const (
	// LevelDebug enables all logs
	LevelDebug LogLevel = "debug"
	// LevelInfo enables info, warning, and error logs
	LevelInfo LogLevel = "info"
	// LevelProgress enables progress, warning, and error logs (default)
	LevelProgress LogLevel = "progress"
	// LevelMinimal enables only warning and error logs
	LevelMinimal LogLevel = "minimal"
	// LevelWarn enables only warning and error logs (alias for minimal)
	LevelWarn LogLevel = "warn"
	// LevelError enables only error logs
	LevelError LogLevel = "error"
)

// global logger instance
var (
	globalLogger *zap.SugaredLogger
	globalMutex  sync.RWMutex
)

// Config holds logger configuration
type Config struct {
	Level  LogLevel
	Format string // "console" or "json" (future)
}

// DefaultConfig returns the default logger configuration
func DefaultConfig() Config {
	return Config{
		Level:  LevelProgress,
		Format: "console",
	}
}

// Init initializes the global logger with the given configuration
func Init(cfg Config) error {
	// Map log level to zap level
	zapLevel := mapLevelToZapLevel(cfg.Level)

	logger := createLoggerWithLevel(zapLevel)

	globalMutex.Lock()
	defer globalMutex.Unlock()
	globalLogger = logger.Sugar()
	return nil
}

// mapLevelToZapLevel maps our log level to zap level
func mapLevelToZapLevel(level LogLevel) zapcore.Level {
	switch level {
	case LevelDebug:
		return zapcore.DebugLevel
	case LevelInfo:
		return zapcore.InfoLevel
	case LevelProgress:
		// Progress maps to Info level for now
		// We could create a custom level in the future
		return zapcore.InfoLevel
	case LevelMinimal, LevelWarn:
		return zapcore.WarnLevel
	case LevelError:
		return zapcore.ErrorLevel
	default:
		return zapcore.InfoLevel
	}
}

// buildEncoderConfig creates the encoder configuration for console output
func buildEncoderConfig() zapcore.EncoderConfig {
	return zapcore.EncoderConfig{
		TimeKey:        "T",
		LevelKey:       "L",
		NameKey:        "N",
		CallerKey:      "C",
		FunctionKey:    zapcore.OmitKey,
		MessageKey:     "M",
		StacktraceKey:  "S",
		LineEnding:     zapcore.DefaultLineEnding,
		EncodeLevel:    zapcore.CapitalColorLevelEncoder,
		EncodeTime:     zapcore.ISO8601TimeEncoder,
		EncodeDuration: zapcore.StringDurationEncoder,
		EncodeCaller:   zapcore.ShortCallerEncoder,
	}
}

// Get returns the global logger
// If not initialized, it initializes with default config
func Get() *zap.SugaredLogger {
	globalMutex.RLock()
	logger := globalLogger
	globalMutex.RUnlock()

	if logger != nil {
		return logger
	}

	// Initialize with default config if not yet initialized
	// We need to do initialization without holding the lock to avoid deadlock
	// since Init() also acquires the lock
	loggerToSet := createLogger(DefaultConfig())

	globalMutex.Lock()
	defer globalMutex.Unlock()

	// Check again in case another goroutine initialized while we were creating
	if globalLogger != nil {
		return globalLogger
	}

	globalLogger = loggerToSet
	return globalLogger
}

// createLogger creates a new logger with the given config without acquiring locks
// This is used internally by Get() to avoid deadlock
func createLogger(cfg Config) *zap.SugaredLogger {
	zapLevel := mapLevelToZapLevel(cfg.Level)
	return createLoggerWithLevel(zapLevel).Sugar()
}

// createLoggerWithLevel creates a new logger with the given zap level
func createLoggerWithLevel(zapLevel zapcore.Level) *zap.Logger {
	// Build encoder config for human-readable console output
	encoderConfig := buildEncoderConfig()

	// Create console encoder
	encoder := zapcore.NewConsoleEncoder(encoderConfig)

	// Write to stdout
	writeSyncer := zapcore.AddSync(os.Stdout)

	// Create core
	core := zapcore.NewCore(encoder, writeSyncer, zapLevel)

	// Create logger with caller tracking enabled
	logger := zap.New(core, zap.AddCaller(), zap.AddCallerSkip(1), zap.AddStacktrace(zapcore.ErrorLevel))

	return logger
}

// Debug logs a debug message
func Debug(msg string, args ...interface{}) {
	Get().Debugw(msg, args...)
}

// Debugf logs a formatted debug message
func Debugf(template string, args ...interface{}) {
	Get().Debugf(template, args...)
}

// Info logs an info message
func Info(msg string, args ...interface{}) {
	Get().Infow(msg, args...)
}

// Infof logs a formatted info message
func Infof(template string, args ...interface{}) {
	Get().Infof(template, args...)
}

// Progress logs a progress message (maps to Info level)
func Progress(msg string, args ...interface{}) {
	Get().Infow(msg, args...)
}

// Progressf logs a formatted progress message
func Progressf(template string, args ...interface{}) {
	Get().Infof(template, args...)
}

// Warn logs a warning message
func Warn(msg string, args ...interface{}) {
	Get().Warnw(msg, args...)
}

// Warnf logs a formatted warning message
func Warnf(template string, args ...interface{}) {
	Get().Warnf(template, args...)
}

// Error logs an error message
func Error(msg string, args ...interface{}) {
	Get().Errorw(msg, args...)
}

// Errorf logs a formatted error message
func Errorf(template string, args ...interface{}) {
	Get().Errorf(template, args...)
}

// Fatal logs a fatal message and exits
func Fatal(msg string, args ...interface{}) {
	Get().Fatalw(msg, args...)
}

// Fatalf logs a formatted fatal message and exits
func Fatalf(template string, args ...interface{}) {
	Get().Fatalf(template, args...)
}

// With returns a logger with additional fields
func With(args ...interface{}) *zap.SugaredLogger {
	return Get().With(args...)
}

// Sync flushes any buffered log entries
func Sync() error {
	globalMutex.RLock()
	logger := globalLogger
	globalMutex.RUnlock()

	if logger != nil {
		return logger.Sync()
	}
	return nil
}

// Reset resets the global logger (mainly for testing)
func Reset() {
	globalMutex.Lock()
	defer globalMutex.Unlock()
	if globalLogger != nil {
		_ = globalLogger.Sync()
	}
	globalLogger = nil
}
