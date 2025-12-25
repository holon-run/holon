package github

import (
	"context"
	"fmt"
	"net/url"
	"strconv"
	"strings"
	"time"
)

// ListOptions defines pagination options for listing resources
type ListOptions struct {
	Page     int       `json:"page"`               // Page number (1-based)
	PerPage  int       `json:"per_page"`           // Number of items per page (max 100, default 30)
	Since    time.Time `json:"since,omitempty"`    // Only show items updated after this time
	State    string    `json:"state,omitempty"`    // Filter by state (e.g., "open", "closed", "all")
	Sort     string    `json:"sort,omitempty"`     // Sort field (e.g., "created", "updated")
	Direction string   `json:"direction,omitempty"` // Sort direction (asc, desc)
}

// DefaultListOptions returns default pagination options
func DefaultListOptions() *ListOptions {
	return &ListOptions{
		Page:    1,
		PerPage: 100, // GitHub API max
	}
}

// ApplyToURL applies list options to a URL as query parameters
func (lo *ListOptions) ApplyToURL(baseURL string) string {
	var b strings.Builder
	b.Grow(len(baseURL) + 100) // Pre-allocate for efficiency
	b.WriteString(baseURL)

	sep := "?"
	if lo.Page > 0 {
		b.WriteString(sep)
		b.WriteString("page=")
		b.WriteString(strconv.Itoa(lo.Page))
		sep = "&"
	}
	if lo.PerPage > 0 {
		b.WriteString(sep)
		b.WriteString("per_page=")
		b.WriteString(strconv.Itoa(lo.PerPage))
		sep = "&"
	}
	if !lo.Since.IsZero() {
		b.WriteString(sep)
		b.WriteString("since=")
		b.WriteString(lo.Since.Format(time.RFC3339))
		sep = "&"
	}
	if lo.State != "" {
		b.WriteString(sep)
		b.WriteString("state=")
		b.WriteString(lo.State)
		sep = "&"
	}
	if lo.Sort != "" {
		b.WriteString(sep)
		b.WriteString("sort=")
		b.WriteString(lo.Sort)
		sep = "&"
	}
	if lo.Direction != "" {
		b.WriteString(sep)
		b.WriteString("direction=")
		b.WriteString(lo.Direction)
	}
	return b.String()
}

// Response represents a paginated GitHub API response
type Response struct {
	// The number of items returned in this response
	Count int

	// The total number of items across all pages (if available)
	TotalCount int

	// Information about the next page
	NextPage int

	// Information about the previous page
	PrevPage int

	// The first page number
	FirstPage int

	// The last page number
	LastPage int

	// Rate limit information
	RateLimit *RateLimitStatus
}

// PageResult represents a single page of results
type PageResult struct {
	Items    interface{}  `json:"items"`              // The items in this page
	Response *Response    `json:"response"`           // Response metadata
	HasMore  bool         `json:"has_more"`           // Whether there are more pages
	Error    error        `json:"error,omitempty"`    // Any error that occurred
}

// Paginator handles pagination for GitHub API requests
type Paginator struct {
	client     *Client
	opts       *ListOptions
	maxResults int // Maximum results to fetch (0 = unlimited)
}

// NewPaginator creates a new paginator
func NewPaginator(client *Client, opts *ListOptions) *Paginator {
	if opts == nil {
		opts = DefaultListOptions()
	}
	return &Paginator{
		client: client,
		opts:   opts,
	}
}

// SetMaxResults sets the maximum number of results to fetch
func (p *Paginator) SetMaxResults(max int) {
	p.maxResults = max
}

// FetchAll fetches all pages and returns a combined list
func (p *Paginator) FetchAll(ctx context.Context, urlStr string) ([]interface{}, error) {
	var allItems []interface{}
	page := p.opts.Page
	perPage := p.opts.PerPage

	for {
		// Check max results
		if p.maxResults > 0 && len(allItems) >= p.maxResults {
			break
		}

		// Build URL for this page, preserving existing query parameters
		pageURL, err := addPageParams(urlStr, page, perPage)
		if err != nil {
			return nil, fmt.Errorf("failed to build page URL: %w", err)
		}

		// Fetch page
		req, err := p.client.NewRequest(ctx, "GET", pageURL, nil)
		if err != nil {
			return nil, fmt.Errorf("failed to create request: %w", err)
		}

		resp, err := p.client.Do(req, nil)
		if err != nil {
			return nil, fmt.Errorf("failed to fetch page %d: %w", page, err)
		}
		// Close response on error to avoid resource leak
		defer func() {
			if resp != nil {
				_ = resp.Close()
			}
		}()

		// Parse items from response
		var items []interface{}
		if err := resp.DecodeJSON(&items); err != nil {
			return nil, fmt.Errorf("failed to decode items: %w", err)
		}

		// Add items to results
		allItems = append(allItems, items...)

		// Check if we've fetched all items
		if len(items) < perPage {
			break
		}

		// Check max results
		if p.maxResults > 0 && len(allItems) >= p.maxResults {
			// Trim to max results
			allItems = allItems[:p.maxResults]
			break
		}

		page++
	}

	return allItems, nil
}

// FetchPage fetches a single page of results
func (p *Paginator) FetchPage(ctx context.Context, urlStr string, page int) (*PageResult, error) {
	// Build URL for this page, preserving existing query parameters
	pageURL, err := addPageParams(urlStr, page, p.opts.PerPage)
	if err != nil {
		return nil, fmt.Errorf("failed to build page URL: %w", err)
	}

	req, err := p.client.NewRequest(ctx, "GET", pageURL, nil)
	if err != nil {
		return nil, fmt.Errorf("failed to create request: %w", err)
	}

	resp, err := p.client.Do(req, nil)
	if err != nil {
		return &PageResult{Error: err}, err
	}

	var items []interface{}
	if err := resp.DecodeJSON(&items); err != nil {
		return &PageResult{Error: err}, err
	}

	// Build response metadata
	result := &PageResult{
		Items:   items,
		HasMore: len(items) == p.opts.PerPage,
		Response: &Response{
			Count: len(items),
		},
	}

	// Parse link header for pagination info
	if link := resp.Header.Get("Link"); link != "" {
		pagination := parseLinkHeader(link)
		result.Response.NextPage = pagination.Next
		result.Response.PrevPage = pagination.Prev
		result.Response.FirstPage = pagination.First
		result.Response.LastPage = pagination.Last
	}

	return result, nil
}

// FetchPageChan returns a channel that yields pages as they are fetched
func (p *Paginator) FetchPageChan(ctx context.Context, url string) <-chan *PageResult {
	ch := make(chan *PageResult, 10) // Buffer for better performance

	go func() {
		defer close(ch)

		page := p.opts.Page
		fetched := 0

		for {
			// Check max results
			if p.maxResults > 0 && fetched >= p.maxResults {
				break
			}

			// Check context
			select {
			case <-ctx.Done():
				ch <- &PageResult{Error: ctx.Err()}
				return
			default:
			}

			// Fetch page
			result, err := p.FetchPage(ctx, url, page)
			if err != nil {
				ch <- result
				return
			}

			// Send result
			ch <- result

			// Update counters
			if items, ok := result.Items.([]interface{}); ok {
				fetched += len(items)
			}

			// Check if there are more pages
			if !result.HasMore {
				break
			}

			page++
		}
	}()

	return ch
}

// linkPagination represents pagination information from Link header
type linkPagination struct {
	Next   int
	Prev   int
	First  int
	Last   int
}

// parseLinkHeader parses GitHub's Link header for pagination info
// Format: <url?page=1>; rel="first", <url?page=2>; rel="next", ...
func parseLinkHeader(link string) linkPagination {
	pagination := linkPagination{}

	// Split by comma to get individual links
	links := splitLinkHeader(link)

	for _, l := range links {
		// Extract URL and rel parameter
		url, rel := extractLinkRel(l)
		if url == "" || rel == "" {
			continue
		}

		// Extract page number from URL
		page := extractPageFromURL(url)
		if page == 0 {
			continue
		}

		// Assign to appropriate field
		switch rel {
		case "next":
			pagination.Next = page
		case "prev":
			pagination.Prev = page
		case "first":
			pagination.First = page
		case "last":
			pagination.Last = page
		}
	}

	return pagination
}

// splitLinkHeader splits the Link header by commas, respecting quoted strings
func splitLinkHeader(link string) []string {
	var links []string
	var current strings.Builder
	inQuotes := false

	for _, r := range link {
		switch r {
		case '"':
			inQuotes = !inQuotes
			current.WriteRune(r)
		case ',':
			if !inQuotes {
				links = append(links, strings.TrimSpace(current.String()))
				current.Reset()
			} else {
				current.WriteRune(r)
			}
		default:
			current.WriteRune(r)
		}
	}

	// Add the last link
	if current.Len() > 0 {
		links = append(links, strings.TrimSpace(current.String()))
	}

	return links
}

// extractLinkRel extracts the URL and rel parameter from a single link
func extractLinkRel(link string) (url, rel string) {
	// Format: <url>; rel="type", ...
	parts := strings.SplitN(link, ";", 2)
	if len(parts) < 2 {
		return "", ""
	}

	// Extract URL (remove < >)
	url = strings.TrimSpace(parts[0])
	url = strings.TrimPrefix(url, "<")
	url = strings.TrimSuffix(url, ">")

	// Extract rel parameter
	params := strings.Split(parts[1], ";")
	for _, param := range params {
		param = strings.TrimSpace(param)
		if strings.HasPrefix(param, "rel=") {
			rel = strings.TrimPrefix(param, "rel=")
			rel = strings.Trim(rel, `"`)
			break
		}
	}

	return url, rel
}

// extractPageFromURL extracts the page number from a URL
func extractPageFromURL(urlStr string) int {
	// Parse page parameter from URL query string
	for i := len(urlStr) - 1; i >= 0; i-- {
		if urlStr[i] == '?' || urlStr[i] == '&' {
			if strings.HasPrefix(urlStr[i+1:], "page=") {
				pageStr := urlStr[i+6:]
				// Find end of parameter
				for j := 0; j < len(pageStr); j++ {
					if pageStr[j] == '&' {
						pageStr = pageStr[:j]
						break
					}
				}
				if page, err := strconv.Atoi(pageStr); err == nil {
					return page
				}
			}
			// Don't break - continue searching other parameters
		}
	}
	return 0
}

// addPageParams adds page and per_page parameters to a URL, preserving existing query parameters
func addPageParams(baseURL string, page, perPage int) (string, error) {
	// Parse the base URL
	u, err := url.Parse(baseURL)
	if err != nil {
		return "", fmt.Errorf("failed to parse URL: %w", err)
	}

	// Get existing query values or create new ones
	q := u.Query()

	// Set/update page and per_page parameters
	q.Set("page", strconv.Itoa(page))
	q.Set("per_page", strconv.Itoa(perPage))

	// Encode query parameters back into URL
	u.RawQuery = q.Encode()
	return u.String(), nil
}
