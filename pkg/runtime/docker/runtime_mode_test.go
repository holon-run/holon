package docker

import "testing"

func TestParseRuntimeMode(t *testing.T) {
	tests := []struct {
		name    string
		input   string
		want    RuntimeMode
		wantErr bool
	}{
		{name: "empty defaults to prod", input: "", want: RuntimeModeProd},
		{name: "prod", input: "prod", want: RuntimeModeProd},
		{name: "dev", input: "dev", want: RuntimeModeDev},
		{name: "mixed case", input: "DeV", want: RuntimeModeDev},
		{name: "invalid", input: "foo", wantErr: true},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got, err := ParseRuntimeMode(tt.input)
			if (err != nil) != tt.wantErr {
				t.Fatalf("ParseRuntimeMode() error = %v, wantErr %v", err, tt.wantErr)
			}
			if got != tt.want {
				t.Fatalf("ParseRuntimeMode() = %q, want %q", got, tt.want)
			}
		})
	}
}
