package main

import (
	"bufio"
	"context"
	"fmt"
	"hash/fnv"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"sync"
	"time"

	"github.com/fatih/color"
	"github.com/google/go-github/v57/github"
)

// PRActivity represents a PR with its activity metadata
type PRActivity struct {
	Label     string
	Owner     string
	Repo      string
	PR        *github.PullRequest
	UpdatedAt time.Time
	Issues    []IssueActivity // Related issues linked to this PR
}

// IssueActivity represents an issue with its activity metadata
type IssueActivity struct {
	Label     string
	Owner     string
	Repo      string
	Issue     *github.Issue
	UpdatedAt time.Time
}

// Progress tracks API call progress
type Progress struct {
	current int
	total   int
	mu      sync.Mutex
}

func (p *Progress) increment() {
	p.mu.Lock()
	defer p.mu.Unlock()
	p.current++
}

func (p *Progress) addToTotal(n int) {
	p.mu.Lock()
	defer p.mu.Unlock()
	p.total += n
}

func (p *Progress) display() {
	p.mu.Lock()
	defer p.mu.Unlock()
	percentage := float64(p.current) / float64(p.total) * 100

	// Build the progress bar with colors
	filled := int(percentage / 2) // 50 chars for 100%
	var barContent string
	for i := range 50 {
		if i < filled {
			barContent += "="
		} else if i == filled {
			barContent += ">"
		} else {
			barContent += " "
		}
	}

	// Choose color based on percentage
	var barColor *color.Color
	if percentage < 33 {
		barColor = color.New(color.FgRed)
	} else if percentage < 66 {
		barColor = color.New(color.FgYellow)
	} else {
		barColor = color.New(color.FgGreen)
	}

	// Format: [colored bar] current/total (percentage%)
	fmt.Printf("\r[%s] %s/%s (%s) ",
		barColor.Sprint(barContent),
		color.New(color.FgCyan).Sprint(p.current),
		color.New(color.FgCyan).Sprint(p.total),
		barColor.Sprintf("%.0f%%", percentage))
}

// getLabelColor returns a consistent color for a given label
func getLabelColor(label string) *color.Color {
	labelColors := map[string]*color.Color{
		"Authored":         color.New(color.FgCyan),
		"Mentioned":        color.New(color.FgYellow),
		"Assigned":         color.New(color.FgMagenta),
		"Commented":        color.New(color.FgBlue),
		"Reviewed":         color.New(color.FgGreen),
		"Review Requested": color.New(color.FgRed),
		"Involved":         color.New(color.FgHiBlack),
		"Recent Activity":  color.New(color.FgHiCyan),
	}

	if c, ok := labelColors[label]; ok {
		return c
	}
	return color.New(color.FgWhite)
}

// getUserColor returns a consistent color for a given username using hash
func getUserColor(username string) *color.Color {
	// Use hash to get consistent color for each user
	h := fnv.New32a()
	h.Write([]byte(username))
	hash := h.Sum32()

	// Map to a set of nice visible colors
	colors := []*color.Color{
		color.New(color.FgHiGreen),
		color.New(color.FgHiYellow),
		color.New(color.FgHiBlue),
		color.New(color.FgHiMagenta),
		color.New(color.FgHiCyan),
		color.New(color.FgHiRed),
		color.New(color.FgGreen),
		color.New(color.FgYellow),
		color.New(color.FgBlue),
		color.New(color.FgMagenta),
		color.New(color.FgCyan),
	}

	return colors[hash%uint32(len(colors))]
}

// getStateColor returns a color for a given issue/PR state
func getStateColor(state string) *color.Color {
	switch state {
	case "open":
		return color.New(color.FgGreen)
	case "closed":
		return color.New(color.FgRed)
	case "merged":
		return color.New(color.FgMagenta)
	default:
		return color.New(color.FgWhite)
	}
}

func loadEnvFile(path string) error {
	file, err := os.Open(path)
	if err != nil {
		return err
	}
	defer file.Close()

	scanner := bufio.NewScanner(file)
	for scanner.Scan() {
		line := strings.TrimSpace(scanner.Text())
		if line == "" || strings.HasPrefix(line, "#") {
			continue
		}

		parts := strings.SplitN(line, "=", 2)
		if len(parts) == 2 {
			key := strings.TrimSpace(parts[0])
			value := strings.TrimSpace(parts[1])
			os.Setenv(key, value)
		}
	}

	return scanner.Err()
}

func main() {
	homeDir, err := os.UserHomeDir()
	if err == nil {
		envPath := filepath.Join(homeDir, ".secret", ".gitai.env")
		_ = loadEnvFile(envPath) // Ignore error if file doesn't exist
	}

	// Get GitHub token from environment (try both variable names)
	token := os.Getenv("GITHUB_ACTIVITY_TOKEN")
	if token == "" {
		token = os.Getenv("GITHUB_TOKEN")
	}
	if token == "" {
		fmt.Println("Error: GITHUB_ACTIVITY_TOKEN or GITHUB_TOKEN environment variable is required")
		fmt.Println("\nTo generate a GitHub token:")
		fmt.Println("1. Go to https://github.com/settings/tokens")
		fmt.Println("2. Click 'Generate new token' -> 'Generate new token (classic)'")
		fmt.Println("3. Give it a name and select these scopes: 'repo', 'read:org'")
		fmt.Println("4. Generate and copy the token")
		fmt.Println("5. Export it: export GITHUB_TOKEN=your_token_here")
		fmt.Println("6. Or add it to ~/.secret/.gitai.env")
		os.Exit(1)
	}

	// Parse command line arguments
	var username string
	var includeClosed bool
	var debugMode bool

	// Get username from command line or environment
	username = os.Getenv("GITHUB_USERNAME")
	if username == "" {
		username = os.Getenv("GITHUB_USER")
	}

	// Parse arguments
	for i := 1; i < len(os.Args); i++ {
		arg := os.Args[i]
		if arg == "--closed" {
			includeClosed = true
		} else if arg == "--debug" {
			debugMode = true
		} else if !strings.HasPrefix(arg, "--") {
			username = arg
		}
	}

	if username == "" {
		fmt.Println("Error: Please provide a GitHub username")
		fmt.Println("Usage: gitai [--closed] [--debug] <username>")
		fmt.Println("  --closed: Include closed PRs/issues from the last month")
		fmt.Println("  --debug: Show detailed API progress")
		fmt.Println("Or set GITHUB_USERNAME environment variable")
		fmt.Println("Or add it to ~/.secret/.gitai.env")
		os.Exit(1)
	}

	if debugMode {
		fmt.Printf("Monitoring GitHub PR activity for user: %s\n", username)
		if includeClosed {
			fmt.Println("Including closed items from the last month")
		}
	}
	if debugMode {
		fmt.Println("Debug mode enabled")
	}

	fetchAndDisplayActivity(token, username, includeClosed, debugMode)
}

func checkRateLimit(ctx context.Context, client *github.Client, debugMode bool) error {
	rateLimits, _, err := client.RateLimit.Get(ctx)
	if err != nil {
		return fmt.Errorf("failed to fetch rate limit: %w", err)
	}

	core := rateLimits.Core
	search := rateLimits.Search

	// Display current rate limit status
	if debugMode {
		fmt.Printf("Rate Limits - Core: %d/%d, Search: %d/%d\n",
			core.Remaining, core.Limit,
			search.Remaining, search.Limit)
	}

	// Check if we're hitting the rate limit
	if core.Remaining == 0 {
		resetTime := core.Reset.Time.Sub(time.Now())
		fmt.Printf("WARNING: Core API rate limit exceeded! Resets in %v\n", resetTime.Round(time.Second))
		return fmt.Errorf("rate limit exceeded, resets at %v", core.Reset.Time.Format("15:04:05"))
	}

	if search.Remaining == 0 {
		resetTime := search.Reset.Time.Sub(time.Now())
		fmt.Printf("WARNING: Search API rate limit exceeded! Resets in %v\n", resetTime.Round(time.Second))
		return fmt.Errorf("search rate limit exceeded, resets at %v", search.Reset.Time.Format("15:04:05"))
	}

	// Warn if we're getting low on rate limit (below 20% for core, below 5 for search)
	coreThreshold := core.Limit / 5 // 20%
	if core.Remaining < coreThreshold && core.Remaining > 0 {
		fmt.Printf("WARNING: Core API rate limit running low (%d remaining)\n", core.Remaining)
	}

	if search.Remaining < 5 && search.Remaining > 0 {
		fmt.Printf("WARNING: Search API rate limit running low (%d remaining)\n", search.Remaining)
	}

	return nil
}

func fetchAndDisplayActivity(token, username string, includeClosed bool, debugMode bool) {
	startTime := time.Now()
	ctx := context.Background()
	client := github.NewClient(nil).WithAuthToken(token)

	// Check rate limit before making API calls
	if err := checkRateLimit(ctx, client, debugMode); err != nil {
		fmt.Printf("Skipping this cycle due to rate limit: %v\n", err)
		return
	}
	if debugMode {
		fmt.Println()
	}

	// Track seen PRs to avoid duplicates
	seenPRs := make(map[string]bool)
	var seenPRsMu sync.Mutex
	activities := []PRActivity{}

	// Initialize progress tracker
	// Estimate: 1 rate limit check + 7 PR searches + 3 event pages + 5 issue searches = 16 API calls minimum
	progress := &Progress{current: 0, total: 16}

	if debugMode {
		fmt.Println("Running optimized search queries...")
	} else {
		fmt.Print("Fetching data from GitHub... ")
		progress.display()
	}

	// Calculate dates
	sixMonthsAgo := time.Now().AddDate(0, -6, 0).Format("2006-01-02")
	oneMonthAgo := time.Now().AddDate(0, -1, 0).Format("2006-01-02")

	// Build state and date filters
	var stateFilter, dateFilter string
	if includeClosed {
		// For closed items, show only from last month
		stateFilter = "" // No state filter - include both open and closed
		dateFilter = fmt.Sprintf("updated:>=%s", oneMonthAgo)
	} else {
		// For open items, show from last year
		stateFilter = "state:open"
		dateFilter = fmt.Sprintf("updated:>=%s", sixMonthsAgo)
	}

	// Use GitHub's efficient search API to find all PRs involving the user
	// We use specific queries to properly label each type of involvement

	// Build query with optional state filter
	buildQuery := func(base string) string {
		if stateFilter != "" {
			return fmt.Sprintf("%s %s %s", base, stateFilter, dateFilter)
		}
		return fmt.Sprintf("%s %s", base, dateFilter)
	}

	// Parallelize all PR searches
	var prWg sync.WaitGroup
	var activitiesMu sync.Mutex

	prQueries := []struct {
		query string
		label string
	}{
		{buildQuery(fmt.Sprintf("is:pr author:%s", username)), "Authored"},
		{buildQuery(fmt.Sprintf("is:pr mentions:%s", username)), "Mentioned"},
		{buildQuery(fmt.Sprintf("is:pr assignee:%s", username)), "Assigned"},
		{buildQuery(fmt.Sprintf("is:pr commenter:%s", username)), "Commented"},
		{buildQuery(fmt.Sprintf("is:pr reviewed-by:%s", username)), "Reviewed"},
		{buildQuery(fmt.Sprintf("is:pr review-requested:%s", username)), "Review Requested"},
		{buildQuery(fmt.Sprintf("is:pr involves:%s", username)), "Involved"},
	}

	for _, pq := range prQueries {
		query := pq.query
		label := pq.label
		prWg.Go(func() {
			results := collectSearchResults(ctx, client, query, label, seenPRs, &seenPRsMu, []PRActivity{}, debugMode, progress)
			activitiesMu.Lock()
			activities = append(activities, results...)
			activitiesMu.Unlock()
		})
	}

	// Also run event collection in parallel
	prWg.Go(func() {
		results := collectActivityFromEvents(ctx, client, username, seenPRs, &seenPRsMu, []PRActivity{}, debugMode, progress)
		activitiesMu.Lock()
		activities = append(activities, results...)
		activitiesMu.Unlock()
	})

	prWg.Wait()

	// Now collect issues in parallel
	if debugMode {
		fmt.Println()
		fmt.Println("Running issue search queries...")
	}
	seenIssues := make(map[string]bool)
	var seenIssuesMu sync.Mutex
	issueActivities := []IssueActivity{}

	var issueWg sync.WaitGroup
	var issuesMu sync.Mutex

	issueQueries := []struct {
		query string
		label string
	}{
		{buildQuery(fmt.Sprintf("is:issue author:%s", username)), "Authored"},
		{buildQuery(fmt.Sprintf("is:issue mentions:%s", username)), "Mentioned"},
		{buildQuery(fmt.Sprintf("is:issue assignee:%s", username)), "Assigned"},
		{buildQuery(fmt.Sprintf("is:issue commenter:%s", username)), "Commented"},
		{buildQuery(fmt.Sprintf("is:issue involves:%s", username)), "Involved"},
	}

	for _, iq := range issueQueries {
		query := iq.query
		label := iq.label
		issueWg.Go(func() {
			results := collectIssueSearchResults(ctx, client, query, label, seenIssues, &seenIssuesMu, []IssueActivity{}, debugMode, progress)
			issuesMu.Lock()
			issueActivities = append(issueActivities, results...)
			issuesMu.Unlock()
		})
	}

	issueWg.Wait()

	// Link issues to PRs based on actual cross-references
	// Only link if: PR mentions issue OR issue mentions PR
	// Support many-to-many: an issue can be linked to multiple PRs and vice versa
	if debugMode {
		fmt.Println("Checking cross-references between PRs and issues...")
	}

	// Calculate number of cross-reference checks needed (issues x matching PRs in same repo)
	crossRefChecks := 0
	for j := range issueActivities {
		issue := &issueActivities[j]
		for i := range activities {
			pr := &activities[i]
			if pr.Owner == issue.Owner && pr.Repo == issue.Repo {
				crossRefChecks++
			}
		}
	}

	// Update progress total to include cross-reference checks
	// Each check may do up to 2 API calls (PR comments + issue comments)
	progress.addToTotal(crossRefChecks)
	if !debugMode {
		progress.display()
	}

	linkedIssues := make(map[string]bool) // Track which issues are linked to at least one PR

	var wg sync.WaitGroup

	for j := range issueActivities {
		issue := &issueActivities[j]
		issueKey := fmt.Sprintf("%s/%s#%d", issue.Owner, issue.Repo, issue.Issue.GetNumber())

		for i := range activities {
			pr := &activities[i]
			// Only check PRs in the same repo and same owner
			if pr.Owner == issue.Owner && pr.Repo == issue.Repo {
				wg.Go(func() {
					if areCrossReferenced(ctx, client, pr, issue, debugMode, progress) {
						pr.Issues = append(pr.Issues, *issue)
						linkedIssues[issueKey] = true
						if debugMode {
							fmt.Printf("  Linked %s/%s#%d <-> %s/%s#%d\n",
								pr.Owner, pr.Repo, pr.PR.GetNumber(),
								issue.Owner, issue.Repo, issue.Issue.GetNumber())
						}
					}
				})
			}
		}
	}

	wg.Wait()

	// Collect standalone issues (not linked to any PR)
	standaloneIssues := []IssueActivity{}
	for _, issue := range issueActivities {
		issueKey := fmt.Sprintf("%s/%s#%d", issue.Owner, issue.Repo, issue.Issue.GetNumber())
		if !linkedIssues[issueKey] {
			standaloneIssues = append(standaloneIssues, issue)
		}
	}

	duration := time.Since(startTime)
	if debugMode {
		fmt.Println()
		fmt.Printf("Total fetch time: %v\n", duration.Round(time.Millisecond))
		fmt.Printf("Found %d unique PRs and %d unique issues\n", len(activities), len(issueActivities))
		fmt.Println()
	} else {
		// Clear progress bar and add newline
		fmt.Print("\r" + strings.Repeat(" ", 80) + "\r")
	}

	if len(activities) == 0 && len(standaloneIssues) == 0 {
		fmt.Println("No open activity found")
		return
	}

	// Sort by UpdatedAt descending (newest first)
	sort.Slice(activities, func(i, j int) bool {
		return activities[i].UpdatedAt.After(activities[j].UpdatedAt)
	})
	sort.Slice(standaloneIssues, func(i, j int) bool {
		return standaloneIssues[i].UpdatedAt.After(standaloneIssues[j].UpdatedAt)
	})

	// Separate open and closed/merged PRs
	var openPRs, closedPRs, mergedPRs []PRActivity
	for _, activity := range activities {
		if activity.PR.State != nil && *activity.PR.State == "closed" {
			if activity.PR.Merged != nil && *activity.PR.Merged {
				mergedPRs = append(mergedPRs, activity)
			} else {
				closedPRs = append(closedPRs, activity)
			}
		} else {
			openPRs = append(openPRs, activity)
		}
	}

	// Separate open and closed issues
	var openIssues, closedIssues []IssueActivity
	for _, issue := range standaloneIssues {
		if issue.Issue.State != nil && *issue.Issue.State == "closed" {
			closedIssues = append(closedIssues, issue)
		} else {
			openIssues = append(openIssues, issue)
		}
	}

	// Display open PRs
	if len(openPRs) > 0 {
		titleColor := color.New(color.FgHiGreen, color.Bold)
		fmt.Println(titleColor.Sprint("OPEN PULL REQUESTS:"))
		fmt.Println("------------------------------------------")
		for _, activity := range openPRs {
			displayPR(activity.Label, activity.Owner, activity.Repo, activity.PR)
			// Display related issues under the PR
			if len(activity.Issues) > 0 {
				for _, issue := range activity.Issues {
					displayIssue(issue.Label, issue.Owner, issue.Repo, issue.Issue, true)
				}
			}
		}
	}

	// Display merged PRs
	if len(mergedPRs) > 0 {
		fmt.Println()
		titleColor := color.New(color.FgHiMagenta, color.Bold)
		fmt.Println(titleColor.Sprint("MERGED PULL REQUESTS:"))
		fmt.Println("------------------------------------------")
		for _, activity := range mergedPRs {
			displayPR(activity.Label, activity.Owner, activity.Repo, activity.PR)
			// Display related issues under the PR
			if len(activity.Issues) > 0 {
				for _, issue := range activity.Issues {
					displayIssue(issue.Label, issue.Owner, issue.Repo, issue.Issue, true)
				}
			}
		}
	}

	// Display closed PRs
	if len(closedPRs) > 0 {
		fmt.Println()
		titleColor := color.New(color.FgHiRed, color.Bold)
		fmt.Println(titleColor.Sprint("CLOSED PULL REQUESTS:"))
		fmt.Println("------------------------------------------")
		for _, activity := range closedPRs {
			displayPR(activity.Label, activity.Owner, activity.Repo, activity.PR)
			// Display related issues under the PR
			if len(activity.Issues) > 0 {
				for _, issue := range activity.Issues {
					displayIssue(issue.Label, issue.Owner, issue.Repo, issue.Issue, true)
				}
			}
		}
	}

	// Display open standalone issues
	if len(openIssues) > 0 {
		fmt.Println()
		titleColor := color.New(color.FgHiGreen, color.Bold)
		fmt.Println(titleColor.Sprint("OPEN ISSUES:"))
		fmt.Println("------------------------------------------")
		for _, issue := range openIssues {
			displayIssue(issue.Label, issue.Owner, issue.Repo, issue.Issue, false)
		}
	}

	// Display closed standalone issues
	if len(closedIssues) > 0 {
		fmt.Println()
		titleColor := color.New(color.FgHiRed, color.Bold)
		fmt.Println(titleColor.Sprint("CLOSED ISSUES:"))
		fmt.Println("------------------------------------------")
		for _, issue := range closedIssues {
			displayIssue(issue.Label, issue.Owner, issue.Repo, issue.Issue, false)
		}
	}
}

func areCrossReferenced(ctx context.Context, client *github.Client, pr *PRActivity, issue *IssueActivity, debugMode bool, progress *Progress) bool {
	prNumber := pr.PR.GetNumber()
	issueNumber := issue.Issue.GetNumber()

	if debugMode {
		fmt.Printf("  Checking cross-reference: PR %s/%s#%d <-> Issue %s/%s#%d\n",
			pr.Owner, pr.Repo, prNumber,
			issue.Owner, issue.Repo, issueNumber)
	}

	// Check if PR body mentions the issue (e.g., "fixes #123", "#123", "closes #123")
	prBody := pr.PR.GetBody()
	if mentionsNumber(prBody, issueNumber, pr.Owner, pr.Repo) {
		return true
	}

	// Check if issue body mentions the PR
	issueBody := issue.Issue.GetBody()
	if mentionsNumber(issueBody, prNumber, issue.Owner, issue.Repo) {
		return true
	}

	// Check PR comments for issue mentions
	prComments, _, err := client.PullRequests.ListComments(ctx, pr.Owner, pr.Repo, prNumber, &github.PullRequestListCommentsOptions{
		ListOptions: github.ListOptions{PerPage: 100},
	})

	// Increment progress after API call
	progress.increment()
	if !debugMode {
		progress.display()
	}

	if err == nil {
		for _, comment := range prComments {
			if mentionsNumber(comment.GetBody(), issueNumber, pr.Owner, pr.Repo) {
				return true
			}
		}
	}

	return false
}

// mentionsNumber checks if text contains a reference to a given issue/PR number
// Looks for patterns like: #123, fixes #123, closes #123, resolves #123, etc.
// Also checks for full GitHub URLs like: https://github.com/owner/repo/issues/123
func mentionsNumber(text string, number int, owner string, repo string) bool {
	if text == "" {
		return false
	}

	lowerText := strings.ToLower(text)

	// Check for full GitHub URL patterns
	// Both issues and pull requests can be referenced using /issues/ or /pull/ in the URL
	urlPatterns := []string{
		fmt.Sprintf("github.com/%s/%s/issues/%d", strings.ToLower(owner), strings.ToLower(repo), number),
		fmt.Sprintf("github.com/%s/%s/pull/%d", strings.ToLower(owner), strings.ToLower(repo), number),
	}
	for _, pattern := range urlPatterns {
		if strings.Contains(lowerText, pattern) {
			return true
		}
	}

	// Common shorthand patterns for referencing issues/PRs
	patterns := []string{
		fmt.Sprintf("#%d", number),
		fmt.Sprintf("fixes #%d", number),
		fmt.Sprintf("closes #%d", number),
		fmt.Sprintf("resolves #%d", number),
		fmt.Sprintf("fixed #%d", number),
		fmt.Sprintf("closed #%d", number),
		fmt.Sprintf("resolved #%d", number),
		fmt.Sprintf("fix #%d", number),
		fmt.Sprintf("close #%d", number),
		fmt.Sprintf("resolve #%d", number),
	}

	for _, pattern := range patterns {
		if strings.Contains(lowerText, pattern) {
			return true
		}
	}

	return false
}

func collectActivityFromEvents(ctx context.Context, client *github.Client, username string, seenPRs map[string]bool, seenPRsMu *sync.Mutex, activities []PRActivity, debugMode bool, progress *Progress) []PRActivity {
	// Fetch user's recent events to catch any PR activity
	opts := &github.ListOptions{PerPage: 100}

	if debugMode {
		fmt.Println("Checking recent activity events...")
	}
	totalPRs := 0

	// Get up to 300 recent events (3 pages) to catch recent activity
	for page := range 3 {
		if debugMode {
			fmt.Printf("  [Events] Fetching page %d...\n", page+1)
		}
		events, resp, err := client.Activity.ListEventsPerformedByUser(ctx, username, false, opts)

		// Increment progress after API call
		progress.increment()
		if !debugMode {
			progress.display()
		}

		if err != nil {
			fmt.Printf("Error fetching user events: %v\n", err)
			break
		}

		for _, event := range events {
			// Look for PR-related events
			if event.Type == nil || event.Repo == nil {
				continue
			}

			eventType := *event.Type
			// PR events: PullRequestEvent, PullRequestReviewEvent, PullRequestReviewCommentEvent, IssueCommentEvent
			if eventType == "PullRequestEvent" ||
				eventType == "PullRequestReviewEvent" ||
				eventType == "PullRequestReviewCommentEvent" ||
				eventType == "IssueCommentEvent" {

				// Parse repo owner and name
				repoName := *event.Repo.Name
				parts := strings.Split(repoName, "/")
				if len(parts) != 2 {
					continue
				}
				owner, repo := parts[0], parts[1]

				// Try to extract PR number from the event payload
				var prNumber int
				if eventType == "PullRequestEvent" && event.Payload() != nil {
					if prEvent, ok := event.Payload().(*github.PullRequestEvent); ok && prEvent.PullRequest != nil {
						prNumber = *prEvent.PullRequest.Number
					}
				} else if eventType == "PullRequestReviewEvent" && event.Payload() != nil {
					if reviewEvent, ok := event.Payload().(*github.PullRequestReviewEvent); ok && reviewEvent.PullRequest != nil {
						prNumber = *reviewEvent.PullRequest.Number
					}
				} else if eventType == "PullRequestReviewCommentEvent" && event.Payload() != nil {
					if commentEvent, ok := event.Payload().(*github.PullRequestReviewCommentEvent); ok && commentEvent.PullRequest != nil {
						prNumber = *commentEvent.PullRequest.Number
					}
				} else if eventType == "IssueCommentEvent" && event.Payload() != nil {
					if issueEvent, ok := event.Payload().(*github.IssueCommentEvent); ok && issueEvent.Issue != nil && issueEvent.Issue.IsPullRequest() {
						prNumber = *issueEvent.Issue.Number
					}
				}

				if prNumber > 0 {
					prKey := fmt.Sprintf("%s/%s#%d", owner, repo, prNumber)

					seenPRsMu.Lock()
					seen := seenPRs[prKey]
					if !seen {
						seenPRs[prKey] = true
					}
					seenPRsMu.Unlock()

					if !seen {
						// Fetch the PR details
						pr, _, err := client.PullRequests.Get(ctx, owner, repo, prNumber)
						if err != nil || pr.GetState() != "open" {
							continue
						}

						activities = append(activities, PRActivity{
							Label:     "Recent Activity",
							Owner:     owner,
							Repo:      repo,
							PR:        pr,
							UpdatedAt: pr.GetUpdatedAt().Time,
						})
						totalPRs++
					}
				}
			}
		}

		if resp.NextPage == 0 {
			break
		}
		opts.Page = resp.NextPage
	}

	if debugMode {
		if totalPRs > 0 {
			fmt.Printf("  [Events] Complete: %d PRs found\n", totalPRs)
		} else {
			fmt.Println("  [Events] Complete: no new PRs found")
		}
	}

	return activities
}

func collectSearchResults(ctx context.Context, client *github.Client, query, label string, seenPRs map[string]bool, seenPRsMu *sync.Mutex, activities []PRActivity, debugMode bool, progress *Progress) []PRActivity {
	opts := &github.SearchOptions{
		ListOptions: github.ListOptions{PerPage: 100},
	}

	totalFound := 0

	// Paginate through all results
	page := 1
	for {
		if debugMode {
			fmt.Printf("  [%s] Searching page %d...\n", label, page)
		}
		result, resp, err := client.Search.Issues(ctx, query, opts)

		// Increment progress after API call
		progress.increment()
		if !debugMode {
			progress.display()
		}

		if err != nil {
			fmt.Printf("Error searching '%s': %v\n", query, err)
			if resp != nil {
				fmt.Printf("Rate limit remaining: %d\n", resp.Rate.Remaining)
			}
			return activities
		}

		pageResults := 0
		for _, issue := range result.Issues {
			// Only process issues that are actually PRs
			if issue.PullRequestLinks == nil {
				continue
			}

			// Parse owner/repo from repository URL
			repoURL := *issue.RepositoryURL
			// Extract owner and repo from URL like: https://api.github.com/repos/owner/repo
			parts := strings.Split(repoURL, "/")
			if len(parts) < 2 {
				fmt.Printf("  [%s] Error: Invalid repository URL format: %s\n", label, repoURL)
				continue
			}
			owner := parts[len(parts)-2]
			repo := parts[len(parts)-1]

			prKey := fmt.Sprintf("%s/%s#%d", owner, repo, *issue.Number)

			seenPRsMu.Lock()
			seen := seenPRs[prKey]
			if !seen {
				seenPRs[prKey] = true
			}
			seenPRsMu.Unlock()

			if !seen {
				// Fetch the actual PR to get more details
				pr, _, err := client.PullRequests.Get(ctx, owner, repo, *issue.Number)
				if err != nil {
					// Log the error but still try to show the PR with limited info
					fmt.Printf("  [%s] Warning: Could not fetch details for %s/%s#%d: %v\n", label, owner, repo, *issue.Number, err)

					// Create a minimal PR object from the issue data
					pr = &github.PullRequest{
						Number:    issue.Number,
						Title:     issue.Title,
						State:     issue.State,
						UpdatedAt: issue.UpdatedAt,
						User:      issue.User,
						HTMLURL:   issue.HTMLURL,
					}
				}

				activities = append(activities, PRActivity{
					Label:     label,
					Owner:     owner,
					Repo:      repo,
					PR:        pr,
					UpdatedAt: pr.GetUpdatedAt().Time,
				})
				pageResults++
				totalFound++
			}
		}

		if debugMode {
			fmt.Printf("  [%s] Page %d: found %d new PRs (total: %d)\n", label, page, pageResults, totalFound)
		}

		// Check if there are more pages
		if resp.NextPage == 0 {
			break
		}
		opts.Page = resp.NextPage
		page++
	}

	if debugMode && totalFound > 0 {
		fmt.Printf("  [%s] Complete: %d PRs found\n", label, totalFound)
	}

	return activities
}

func displayPR(label, owner, repo string, pr *github.PullRequest) {
	// Use UpdatedAt as the most recent activity date
	dateStr := "          "
	if pr.UpdatedAt != nil {
		dateStr = pr.UpdatedAt.Format("2006/01/02")
	}

	labelColor := getLabelColor(label)
	userColor := getUserColor(pr.User.GetLogin())

	fmt.Printf("%s %s %s %s/%s#%d - %s\n",
		dateStr,
		labelColor.Sprint(strings.ToUpper(label)),
		userColor.Sprint(pr.User.GetLogin()),
		owner, repo, *pr.Number,
		*pr.Title,
	)
}

func displayIssue(label, owner, repo string, issue *github.Issue, indented bool) {
	// Use UpdatedAt as the most recent activity date
	dateStr := "          "
	if issue.UpdatedAt != nil {
		dateStr = issue.UpdatedAt.Format("2006/01/02")
	}

	indent := ""
	if indented {
		state := strings.ToUpper(*issue.State)
		stateColor := getStateColor(*issue.State)
		indent = fmt.Sprintf("-- %s ", stateColor.Sprint(state))
	}

	labelColor := getLabelColor(label)
	userColor := getUserColor(issue.User.GetLogin())

	fmt.Printf("%s%s %s %s %s/%s#%d - %s\n",
		indent,
		dateStr,
		labelColor.Sprint(strings.ToUpper(label)),
		userColor.Sprint(issue.User.GetLogin()),
		owner, repo, *issue.Number,
		*issue.Title,
	)
}

func collectIssueSearchResults(ctx context.Context, client *github.Client, query, label string, seenIssues map[string]bool, seenIssuesMu *sync.Mutex, issueActivities []IssueActivity, debugMode bool, progress *Progress) []IssueActivity {
	opts := &github.SearchOptions{
		ListOptions: github.ListOptions{PerPage: 100},
	}

	totalFound := 0

	// Paginate through all results
	page := 1
	for {
		if debugMode {
			fmt.Printf("  [%s] Searching page %d...\n", label, page)
		}
		result, resp, err := client.Search.Issues(ctx, query, opts)

		// Increment progress after API call
		progress.increment()
		if !debugMode {
			progress.display()
		}

		if err != nil {
			fmt.Printf("Error searching '%s': %v\n", query, err)
			if resp != nil {
				fmt.Printf("Rate limit remaining: %d\n", resp.Rate.Remaining)
			}
			return issueActivities
		}

		pageResults := 0
		for _, issue := range result.Issues {
			// Skip if this is actually a PR
			if issue.PullRequestLinks != nil {
				continue
			}

			// Parse owner/repo from repository URL
			repoURL := *issue.RepositoryURL
			parts := strings.Split(repoURL, "/")
			if len(parts) < 2 {
				fmt.Printf("  [%s] Error: Invalid repository URL format: %s\n", label, repoURL)
				continue
			}
			owner := parts[len(parts)-2]
			repo := parts[len(parts)-1]

			issueKey := fmt.Sprintf("%s/%s#%d", owner, repo, *issue.Number)

			seenIssuesMu.Lock()
			seen := seenIssues[issueKey]
			if !seen {
				seenIssues[issueKey] = true
			}
			seenIssuesMu.Unlock()

			if !seen {
				issueActivities = append(issueActivities, IssueActivity{
					Label:     label,
					Owner:     owner,
					Repo:      repo,
					Issue:     issue,
					UpdatedAt: issue.GetUpdatedAt().Time,
				})
				pageResults++
				totalFound++
			}
		}

		if debugMode {
			fmt.Printf("  [%s] Page %d: found %d new issues (total: %d)\n", label, page, pageResults, totalFound)
		}

		// Check if there are more pages
		if resp.NextPage == 0 {
			break
		}
		opts.Page = resp.NextPage
		page++
	}

	if debugMode && totalFound > 0 {
		fmt.Printf("  [%s] Complete: %d issues found\n", label, totalFound)
	}

	return issueActivities
}
