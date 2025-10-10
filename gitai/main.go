package main

import (
	"bufio"
	"context"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strings"
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
	// Load env file from ~/.secret/.gitai.env
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
		color.Red("Error: GITHUB_ACTIVITY_TOKEN or GITHUB_TOKEN environment variable is required")
		fmt.Println("\nTo generate a GitHub token:")
		fmt.Println("1. Go to https://github.com/settings/tokens")
		fmt.Println("2. Click 'Generate new token' -> 'Generate new token (classic)'")
		fmt.Println("3. Give it a name and select these scopes: 'repo', 'read:org'")
		fmt.Println("4. Generate and copy the token")
		fmt.Println("5. Export it: export GITHUB_ACTIVITY_TOKEN=your_token_here")
		fmt.Println("6. Or add it to ~/.secret/.gitai.env")
		os.Exit(1)
	}

	// Get username from command line or environment
	username := os.Getenv("GITHUB_USERNAME")
	if username == "" {
		username = os.Getenv("GITHUB_USER")
	}
	if len(os.Args) > 1 {
		username = os.Args[1]
	}
	if username == "" {
		color.Red("Error: Please provide a GitHub username")
		fmt.Println("Usage: gitai <username>")
		fmt.Println("Or set GITHUB_USERNAME environment variable")
		fmt.Println("Or add it to ~/.secret/.gitai.env")
		os.Exit(1)
	}

	color.Cyan("🔍 Monitoring GitHub PR activity for user: %s\n", username)
	color.Yellow("Press Ctrl+C to stop\n\n")

	// Poll every 2 minutes
	for {
		fetchAndDisplayActivity(token, username)
		time.Sleep(2 * time.Minute)
		fmt.Println()
		color.Cyan("🔄 Refreshing...\n")
	}
}

func checkRateLimit(ctx context.Context, client *github.Client) error {
	rateLimits, _, err := client.RateLimit.Get(ctx)
	if err != nil {
		return fmt.Errorf("failed to fetch rate limit: %w", err)
	}

	core := rateLimits.Core
	search := rateLimits.Search

	// Display current rate limit status
	color.HiBlack("📊 Rate Limits - Core: %d/%d, Search: %d/%d",
		core.Remaining, core.Limit,
		search.Remaining, search.Limit)

	// Check if we're hitting the rate limit
	if core.Remaining == 0 {
		resetTime := core.Reset.Time.Sub(time.Now())
		color.Red("⚠️  Core API rate limit exceeded! Resets in %v", resetTime.Round(time.Second))
		return fmt.Errorf("rate limit exceeded, resets at %v", core.Reset.Time.Format("15:04:05"))
	}

	if search.Remaining == 0 {
		resetTime := search.Reset.Time.Sub(time.Now())
		color.Red("⚠️  Search API rate limit exceeded! Resets in %v", resetTime.Round(time.Second))
		return fmt.Errorf("search rate limit exceeded, resets at %v", search.Reset.Time.Format("15:04:05"))
	}

	// Warn if we're getting low on rate limit (below 20% for core, below 5 for search)
	coreThreshold := core.Limit / 5 // 20%
	if core.Remaining < coreThreshold && core.Remaining > 0 {
		color.Yellow("⚠️  Warning: Core API rate limit running low (%d remaining)", core.Remaining)
	}

	if search.Remaining < 5 && search.Remaining > 0 {
		color.Yellow("⚠️  Warning: Search API rate limit running low (%d remaining)", search.Remaining)
	}

	return nil
}

func fetchAndDisplayActivity(token, username string) {
	startTime := time.Now()
	ctx := context.Background()
	client := github.NewClient(nil).WithAuthToken(token)

	// Check rate limit before making API calls
	if err := checkRateLimit(ctx, client); err != nil {
		color.Red("Skipping this cycle due to rate limit: %v", err)
		return
	}
	fmt.Println()

	// Track seen PRs to avoid duplicates
	seenPRs := make(map[string]bool)
	activities := []PRActivity{}

	color.Cyan("🔎 Running optimized search queries...")

	// Use GitHub's efficient search API to find all PRs involving the user
	// We use specific queries to properly label each type of involvement

	// 1. PRs authored by the user
	searchQuery := fmt.Sprintf("is:pr author:%s state:open", username)
	activities = collectSearchResults(ctx, client, searchQuery, "✏️  Authored", seenPRs, activities)

	// 2. PRs where user is mentioned
	searchQuery = fmt.Sprintf("is:pr mentions:%s state:open", username)
	activities = collectSearchResults(ctx, client, searchQuery, "💬 Mentioned", seenPRs, activities)

	// 3. PRs where user is assigned
	searchQuery = fmt.Sprintf("is:pr assignee:%s state:open", username)
	activities = collectSearchResults(ctx, client, searchQuery, "👤 Assigned", seenPRs, activities)

	// 4. PRs where user commented
	searchQuery = fmt.Sprintf("is:pr commenter:%s state:open", username)
	activities = collectSearchResults(ctx, client, searchQuery, "💭 Commented", seenPRs, activities)

	// 5. PRs where user reviewed
	searchQuery = fmt.Sprintf("is:pr reviewed-by:%s state:open", username)
	activities = collectSearchResults(ctx, client, searchQuery, "👁️  Reviewed", seenPRs, activities)

	// 6. PRs where user is requested for review
	searchQuery = fmt.Sprintf("is:pr review-requested:%s state:open", username)
	activities = collectSearchResults(ctx, client, searchQuery, "🔔 Review Requested", seenPRs, activities)

	// 7. Main query as catch-all for any other involvement
	searchQuery = fmt.Sprintf("is:pr involves:%s state:open", username)
	activities = collectSearchResults(ctx, client, searchQuery, "🔗 Involved", seenPRs, activities)

	// 8. Check user's recent activity events to catch any missed PR interactions
	activities = collectActivityFromEvents(ctx, client, username, seenPRs, activities)

	// Now collect issues
	fmt.Println()
	color.Cyan("🔎 Running issue search queries...")
	seenIssues := make(map[string]bool)
	issueActivities := []IssueActivity{}

	// Use GitHub's search API to find all issues involving the user
	issueActivities = collectIssueSearchResults(ctx, client, fmt.Sprintf("is:issue author:%s state:open", username), "✏️  Authored", seenIssues, issueActivities)
	issueActivities = collectIssueSearchResults(ctx, client, fmt.Sprintf("is:issue mentions:%s state:open", username), "💬 Mentioned", seenIssues, issueActivities)
	issueActivities = collectIssueSearchResults(ctx, client, fmt.Sprintf("is:issue assignee:%s state:open", username), "👤 Assigned", seenIssues, issueActivities)
	issueActivities = collectIssueSearchResults(ctx, client, fmt.Sprintf("is:issue commenter:%s state:open", username), "💭 Commented", seenIssues, issueActivities)
	issueActivities = collectIssueSearchResults(ctx, client, fmt.Sprintf("is:issue involves:%s state:open", username), "🔗 Involved", seenIssues, issueActivities)

	// Link issues to PRs based on actual cross-references
	// Only link if: PR mentions issue OR issue mentions PR
	// Support many-to-many: an issue can be linked to multiple PRs and vice versa
	color.Cyan("🔗 Checking cross-references between PRs and issues...")
	linkedIssues := make(map[string]bool) // Track which issues are linked to at least one PR

	for j := range issueActivities {
		issue := &issueActivities[j]
		issueKey := fmt.Sprintf("%s/%s#%d", issue.Owner, issue.Repo, issue.Issue.GetNumber())

		for i := range activities {
			pr := &activities[i]
			// Only check PRs in the same repo and same owner
			if pr.Owner == issue.Owner && pr.Repo == issue.Repo {
				if areCrossReferenced(ctx, client, pr, issue) {
					pr.Issues = append(pr.Issues, *issue)
					linkedIssues[issueKey] = true
					color.HiBlack("  ✓ Linked %s/%s#%d ↔ %s/%s#%d",
						pr.Owner, pr.Repo, pr.PR.GetNumber(),
						issue.Owner, issue.Repo, issue.Issue.GetNumber())
				}
			}
		}
	}

	// Collect standalone issues (not linked to any PR)
	standaloneIssues := []IssueActivity{}
	for _, issue := range issueActivities {
		issueKey := fmt.Sprintf("%s/%s#%d", issue.Owner, issue.Repo, issue.Issue.GetNumber())
		if !linkedIssues[issueKey] {
			standaloneIssues = append(standaloneIssues, issue)
		}
	}

	duration := time.Since(startTime)
	fmt.Println()
	color.Cyan("⏱️  Total fetch time: %v", duration.Round(time.Millisecond))
	color.Green("✨ Found %d unique PRs and %d unique issues", len(activities), len(issueActivities))
	fmt.Println()

	if len(activities) == 0 && len(standaloneIssues) == 0 {
		color.Yellow("No open activity found")
		return
	}

	// Sort by UpdatedAt descending (newest first)
	sort.Slice(activities, func(i, j int) bool {
		return activities[i].UpdatedAt.After(activities[j].UpdatedAt)
	})
	sort.Slice(standaloneIssues, func(i, j int) bool {
		return standaloneIssues[i].UpdatedAt.After(standaloneIssues[j].UpdatedAt)
	})

	// Display sorted activities
	if len(activities) > 0 {
		color.Cyan("📋 Pull Requests:")
		for _, activity := range activities {
			displayPR(activity.Label, activity.Owner, activity.Repo, activity.PR)
			// Display related issues under the PR
			if len(activity.Issues) > 0 {
				for _, issue := range activity.Issues {
					displayIssue(issue.Label, issue.Owner, issue.Repo, issue.Issue, true)
				}
			}
		}
	}

	// Display standalone issues
	if len(standaloneIssues) > 0 {
		fmt.Println()
		color.Cyan("📋 Issues (standalone):")
		for _, issue := range standaloneIssues {
			displayIssue(issue.Label, issue.Owner, issue.Repo, issue.Issue, false)
		}
	}
}

func areCrossReferenced(ctx context.Context, client *github.Client, pr *PRActivity, issue *IssueActivity) bool {
	prNumber := pr.PR.GetNumber()
	issueNumber := issue.Issue.GetNumber()

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
	prComments, _, err := client.Issues.ListComments(ctx, pr.Owner, pr.Repo, prNumber, &github.IssueListCommentsOptions{
		ListOptions: github.ListOptions{PerPage: 100},
	})
	if err == nil {
		for _, comment := range prComments {
			if mentionsNumber(comment.GetBody(), issueNumber, pr.Owner, pr.Repo) {
				return true
			}
		}
	}

	// Check issue comments for PR mentions
	issueComments, _, err := client.Issues.ListComments(ctx, issue.Owner, issue.Repo, issueNumber, &github.IssueListCommentsOptions{
		ListOptions: github.ListOptions{PerPage: 100},
	})
	if err == nil {
		for _, comment := range issueComments {
			if mentionsNumber(comment.GetBody(), prNumber, issue.Owner, issue.Repo) {
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

func collectActivityFromEvents(ctx context.Context, client *github.Client, username string, seenPRs map[string]bool, activities []PRActivity) []PRActivity {
	// Fetch user's recent events to catch any PR activity
	opts := &github.ListOptions{PerPage: 100}

	color.Cyan("📅 Checking recent activity events...")
	totalPRs := 0

	// Get up to 300 recent events (3 pages) to catch recent activity
	for page := range 3 {
		color.HiBlack("  [Events] Fetching page %d...", page+1)
		events, resp, err := client.Activity.ListEventsPerformedByUser(ctx, username, false, opts)
		if err != nil {
			color.Red("Error fetching user events: %v", err)
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
					if !seenPRs[prKey] {
						// Fetch the PR details
						pr, _, err := client.PullRequests.Get(ctx, owner, repo, prNumber)
						if err != nil || pr.GetState() != "open" {
							continue
						}

						seenPRs[prKey] = true
						activities = append(activities, PRActivity{
							Label:     "🔔 Recent Activity",
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

	if totalPRs > 0 {
		color.Green("  [Events] ✓ Complete: %d PRs found", totalPRs)
	} else {
		color.HiBlack("  [Events] ✓ Complete: no new PRs found")
	}

	return activities
}

func collectSearchResults(ctx context.Context, client *github.Client, query, label string, seenPRs map[string]bool, activities []PRActivity) []PRActivity {
	opts := &github.SearchOptions{
		ListOptions: github.ListOptions{PerPage: 100},
	}

	totalFound := 0

	// Paginate through all results
	page := 1
	for {
		color.HiBlack("  [%s] Searching page %d...", label, page)
		result, resp, err := client.Search.Issues(ctx, query, opts)
		if err != nil {
			color.Red("Error searching '%s': %v", query, err)
			if resp != nil {
				color.Red("Rate limit remaining: %d", resp.Rate.Remaining)
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
				color.Red("  [%s] Error: Invalid repository URL format: %s", label, repoURL)
				continue
			}
			owner := parts[len(parts)-2]
			repo := parts[len(parts)-1]

			prKey := fmt.Sprintf("%s/%s#%d", owner, repo, *issue.Number)
			if !seenPRs[prKey] {
				seenPRs[prKey] = true

				// Fetch the actual PR to get more details
				pr, _, err := client.PullRequests.Get(ctx, owner, repo, *issue.Number)
				if err != nil {
					// Log the error but still try to show the PR with limited info
					color.Yellow("  [%s] Warning: Could not fetch details for %s/%s#%d: %v", label, owner, repo, *issue.Number, err)

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

		color.HiBlack("  [%s] Page %d: found %d new PRs (total: %d)", label, page, pageResults, totalFound)

		// Check if there are more pages
		if resp.NextPage == 0 {
			break
		}
		opts.Page = resp.NextPage
		page++
	}

	if totalFound > 0 {
		color.Green("  [%s] ✓ Complete: %d PRs found", label, totalFound)
	}

	return activities
}

func displayPR(label, owner, repo string, pr *github.PullRequest) {
	green := color.New(color.FgGreen).SprintFunc()
	yellow := color.New(color.FgYellow).SprintFunc()
	cyan := color.New(color.FgCyan).SprintFunc()
	gray := color.New(color.FgHiBlack).SprintFunc()

	status := "●"
	if pr.Draft != nil && *pr.Draft {
		status = yellow("◐")
	} else {
		status = green("●")
	}

	// Use UpdatedAt as the most recent activity date
	dateStr := "          "
	if pr.UpdatedAt != nil {
		dateStr = pr.UpdatedAt.Format("2006/01/02")
	}

	fmt.Printf("%s %s %s %s %s/%s#%d - %s\n",
		gray(dateStr),
		status,
		cyan(label),
		yellow(pr.User.GetLogin()),
		owner, repo, *pr.Number,
		*pr.Title,
	)
}

func displayIssue(label, owner, repo string, issue *github.Issue, indented bool) {
	green := color.New(color.FgGreen).SprintFunc()
	cyan := color.New(color.FgCyan).SprintFunc()
	yellow := color.New(color.FgYellow).SprintFunc()
	gray := color.New(color.FgHiBlack).SprintFunc()

	status := green("●")

	// Use UpdatedAt as the most recent activity date
	dateStr := "          "
	if issue.UpdatedAt != nil {
		dateStr = issue.UpdatedAt.Format("2006/01/02")
	}

	indent := ""
	if indented {
		indent = "  "
	}

	fmt.Printf("%s%s %s %s %s %s/%s#%d - %s\n",
		indent,
		gray(dateStr),
		status,
		cyan(label),
		yellow(issue.User.GetLogin()),
		owner, repo, *issue.Number,
		*issue.Title,
	)
}

func collectIssueSearchResults(ctx context.Context, client *github.Client, query, label string, seenIssues map[string]bool, issueActivities []IssueActivity) []IssueActivity {
	opts := &github.SearchOptions{
		ListOptions: github.ListOptions{PerPage: 100},
	}

	totalFound := 0

	// Paginate through all results
	page := 1
	for {
		color.HiBlack("  [%s] Searching page %d...", label, page)
		result, resp, err := client.Search.Issues(ctx, query, opts)
		if err != nil {
			color.Red("Error searching '%s': %v", query, err)
			if resp != nil {
				color.Red("Rate limit remaining: %d", resp.Rate.Remaining)
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
				color.Red("  [%s] Error: Invalid repository URL format: %s", label, repoURL)
				continue
			}
			owner := parts[len(parts)-2]
			repo := parts[len(parts)-1]

			issueKey := fmt.Sprintf("%s/%s#%d", owner, repo, *issue.Number)
			if !seenIssues[issueKey] {
				seenIssues[issueKey] = true

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

		color.HiBlack("  [%s] Page %d: found %d new issues (total: %d)", label, page, pageResults, totalFound)

		// Check if there are more pages
		if resp.NextPage == 0 {
			break
		}
		opts.Page = resp.NextPage
		page++
	}

	if totalFound > 0 {
		color.Green("  [%s] ✓ Complete: %d issues found", label, totalFound)
	}

	return issueActivities
}
