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

	color.Cyan("🔎 Running search queries...")

	// Comprehensive search strategies to catch ALL PR activity
	// Using multiple overlapping searches to ensure nothing is missed

	// 1. PRs created by user
	searchQuery := fmt.Sprintf("is:pr author:%s state:open", username)
	activities = collectSearchResults(ctx, client, searchQuery, "✏️  Created", seenPRs, activities)

	// 2. PRs where user is mentioned
	searchQuery = fmt.Sprintf("is:pr mentions:%s state:open", username)
	activities = collectSearchResults(ctx, client, searchQuery, "💬 Mentioned", seenPRs, activities)

	// 3. PRs where user is assigned
	searchQuery = fmt.Sprintf("is:pr assignee:%s state:open", username)
	activities = collectSearchResults(ctx, client, searchQuery, "👤 Assigned", seenPRs, activities)

	// 4. PRs where user reviewed
	searchQuery = fmt.Sprintf("is:pr reviewed-by:%s state:open", username)
	activities = collectSearchResults(ctx, client, searchQuery, "👁️  Reviewed", seenPRs, activities)

	// 5. PRs where user requested for review
	searchQuery = fmt.Sprintf("is:pr review-requested:%s state:open", username)
	activities = collectSearchResults(ctx, client, searchQuery, "🔔 Review Requested", seenPRs, activities)

	// 6. PRs where user commented
	searchQuery = fmt.Sprintf("is:pr commenter:%s state:open", username)
	activities = collectSearchResults(ctx, client, searchQuery, "💭 Commented", seenPRs, activities)

	// 7. PRs involving the user in any way (comprehensive catch-all)
	searchQuery = fmt.Sprintf("is:pr involves:%s state:open", username)
	activities = collectSearchResults(ctx, client, searchQuery, "🔗 Involved", seenPRs, activities)

	// 8. PRs where user is a team member (for org repos)
	// searchQuery = fmt.Sprintf("is:pr team-review-requested:%s state:open", username)
	// activities = collectSearchResults(ctx, client, searchQuery, "👥 Team Review", seenPRs, activities)

	// 9. PRs in repos the user has contributed to (recently active)
	searchQuery = fmt.Sprintf("is:pr user:%s state:open", username)
	activities = collectSearchResults(ctx, client, searchQuery, "🤝 Contributor", seenPRs, activities)

	// 10. Draft PRs created by user (in case they're filtered differently)
	searchQuery = fmt.Sprintf("is:pr author:%s state:open draft:true", username)
	activities = collectSearchResults(ctx, client, searchQuery, "📝 Draft", seenPRs, activities)

	// 11. Check user's recent activity events to catch any PR interactions
	activities = collectActivityFromEvents(ctx, client, username, seenPRs, activities)

	// 12. Check repositories where user has push access (includes private repos and org repos)
	activities = collectFromAccessibleRepos(ctx, client, username, seenPRs, activities)

	// 13. Get user's repositories and check for PRs (using non-deprecated API) with pagination
	color.Cyan("🏠 Checking user's own repositories...")
	repoOpts := &github.RepositoryListByUserOptions{
		ListOptions: github.ListOptions{PerPage: 100},
	}

	ownRepoPage := 1
	for {
		color.HiBlack("  [Own Repos] Fetching page %d...", ownRepoPage)
		repos, resp, err := client.Repositories.ListByUser(ctx, username, repoOpts)
		if err != nil {
			color.Red("Error fetching repositories: %v", err)
			break
		}

		for _, repo := range repos {
			prOpts := &github.PullRequestListOptions{
				State:       "open",
				ListOptions: github.ListOptions{PerPage: 100},
			}

			// Paginate through PRs in this repo
			for {
				prs, prResp, err := client.PullRequests.List(ctx, username, *repo.Name, prOpts)
				if err != nil {
					break
				}

				for _, pr := range prs {
					prKey := fmt.Sprintf("%s/%s#%d", username, *repo.Name, *pr.Number)
					if !seenPRs[prKey] {
						seenPRs[prKey] = true
						activities = append(activities, PRActivity{
							Label:     "🏠 Own Repo",
							Owner:     username,
							Repo:      *repo.Name,
							PR:        pr,
							UpdatedAt: pr.GetUpdatedAt().Time,
						})
					}
				}

				if prResp.NextPage == 0 {
					break
				}
				prOpts.Page = prResp.NextPage
			}
		}

		if resp.NextPage == 0 {
			break
		}
		repoOpts.Page = resp.NextPage
		ownRepoPage++
	}

	// 14. Check user's organizations and their repositories
	color.Cyan("🏢 Checking organizations...")
	orgOpts := &github.ListOptions{PerPage: 100}
	orgPage := 1
	totalOrgs := 0
	for {
		color.HiBlack("  [Orgs] Fetching page %d...", orgPage)
		orgs, resp, err := client.Organizations.List(ctx, username, orgOpts)
		if err != nil {
			// Not an error if user has no orgs or we can't access them
			color.HiBlack("  [Orgs] No organizations or unable to access")
			break
		}

		totalOrgs += len(orgs)
		for _, org := range orgs {
			// Search for PRs in this org's repos involving the user
			orgSearchQuery := fmt.Sprintf("is:pr org:%s involves:%s state:open", org.GetLogin(), username)
			activities = collectSearchResults(ctx, client, orgSearchQuery, "🏢 Org", seenPRs, activities)
		}

		if resp.NextPage == 0 {
			break
		}
		orgOpts.Page = resp.NextPage
		orgPage++
	}
	if totalOrgs > 0 {
		color.Green("  [Orgs] ✓ Complete: checked %d organizations", totalOrgs)
	}

	duration := time.Since(startTime)
	fmt.Println()
	color.Cyan("⏱️  Total fetch time: %v", duration.Round(time.Millisecond))
	color.Green("✨ Found %d unique PRs", len(activities))
	fmt.Println()

	if len(activities) == 0 {
		color.Yellow("No open PR activity found")
		return
	}

	// Sort by UpdatedAt descending (newest first)
	sort.Slice(activities, func(i, j int) bool {
		return activities[i].UpdatedAt.After(activities[j].UpdatedAt)
	})

	// Display sorted activities
	color.Cyan("📋 Pull Requests:")
	for _, activity := range activities {
		displayPR(activity.Label, activity.Owner, activity.Repo, activity.PR)
	}
}

func collectFromAccessibleRepos(ctx context.Context, client *github.Client, username string, seenPRs map[string]bool, activities []PRActivity) []PRActivity {
	// List ALL repositories the authenticated user has access to
	// This includes: owned repos, org repos, repos with collaborator access, private repos
	opts := &github.RepositoryListOptions{
		ListOptions: github.ListOptions{PerPage: 100},
		Visibility:  "all", // all, public, private
		Affiliation: "owner,collaborator,organization_member", // all types of access
		Sort:        "updated",
	}

	color.Cyan("🔍 Scanning accessible repositories...")
	repoPage := 1
	totalRepos := 0
	totalPRs := 0

	for {
		color.HiBlack("  [Access] Fetching repo page %d...", repoPage)
		repos, resp, err := client.Repositories.List(ctx, "", opts)
		if err != nil {
			color.Red("Error fetching accessible repositories: %v", err)
			break
		}

		color.HiBlack("  [Access] Page %d: checking %d repositories", repoPage, len(repos))
		totalRepos += len(repos)

		for _, repo := range repos {
			// Skip archived repos
			if repo.GetArchived() {
				continue
			}

			owner := repo.GetOwner().GetLogin()
			repoName := repo.GetName()

			prOpts := &github.PullRequestListOptions{
				State:       "open",
				ListOptions: github.ListOptions{PerPage: 100},
			}

			// Paginate through PRs in this repo
			prPage := 1
			repoHasPRs := false
			for {
				prs, prResp, err := client.PullRequests.List(ctx, owner, repoName, prOpts)
				if err != nil {
					break
				}

				if len(prs) > 0 {
					if !repoHasPRs {
						color.HiBlack("    → %s/%s: checking %d open PRs", owner, repoName, len(prs))
						repoHasPRs = true
					}
				}

				for _, pr := range prs {
					prKey := fmt.Sprintf("%s/%s#%d", owner, repoName, *pr.Number)
					if seenPRs[prKey] {
						continue
					}

					// Check if this PR involves the user in any way
					involvesUser := false
					prDetails, _, err := client.PullRequests.Get(ctx, owner, repoName, *pr.Number)
					if err != nil {
						continue
					}

					// Check author
					if prDetails.User != nil && prDetails.User.GetLogin() == username {
						involvesUser = true
					}

					// Check assignees
					for _, assignee := range prDetails.Assignees {
						if assignee.GetLogin() == username {
							involvesUser = true
							break
						}
					}

					// Check requested reviewers
					for _, reviewer := range prDetails.RequestedReviewers {
						if reviewer.GetLogin() == username {
							involvesUser = true
							break
						}
					}

					// Check reviews
					if !involvesUser {
						reviews, _, err := client.PullRequests.ListReviews(ctx, owner, repoName, *pr.Number, nil)
						if err == nil {
							for _, review := range reviews {
								if review.User != nil && review.User.GetLogin() == username {
									involvesUser = true
									break
								}
							}
						}
					}

					// Check comments
					if !involvesUser {
						comments, _, err := client.Issues.ListComments(ctx, owner, repoName, *pr.Number, nil)
						if err == nil {
							for _, comment := range comments {
								if comment.User != nil && comment.User.GetLogin() == username {
									involvesUser = true
									break
								}
							}
						}
					}

					if involvesUser {
						seenPRs[prKey] = true
						activities = append(activities, PRActivity{
							Label:     "🔍 Access",
							Owner:     owner,
							Repo:      repoName,
							PR:        prDetails,
							UpdatedAt: prDetails.GetUpdatedAt().Time,
						})
						totalPRs++
					}
				}

				if prResp.NextPage == 0 {
					break
				}
				prOpts.Page = prResp.NextPage
				prPage++
			}
		}

		if resp.NextPage == 0 {
			break
		}
		opts.Page = resp.NextPage
		repoPage++
	}

	color.Green("  [Access] ✓ Complete: scanned %d repositories, found %d relevant PRs", totalRepos, totalPRs)

	return activities
}

func collectActivityFromEvents(ctx context.Context, client *github.Client, username string, seenPRs map[string]bool, activities []PRActivity) []PRActivity {
	// Fetch user's recent events to catch any PR activity
	opts := &github.ListOptions{PerPage: 100}

	color.Cyan("📅 Checking recent activity events...")
	totalPRs := 0

	// Get up to 300 recent events (3 pages) to catch recent activity
	for page := 0; page < 3; page++ {
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
			if issue.PullRequestLinks == nil {
				continue
			}

			// Parse owner/repo from repository URL
			repoURL := *issue.RepositoryURL
			// Extract owner and repo from URL like: https://api.github.com/repos/owner/repo
			var owner, repo string
			fmt.Sscanf(repoURL, "https://api.github.com/repos/%s/%s", &owner, &repo)

			prKey := fmt.Sprintf("%s/%s#%d", owner, repo, *issue.Number)
			if !seenPRs[prKey] {
				seenPRs[prKey] = true

				// Fetch the actual PR to get more details
				pr, _, err := client.PullRequests.Get(ctx, owner, repo, *issue.Number)
				if err != nil {
					// Skip if we can't get PR details
					continue
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
	activityDate := ""
	if pr.UpdatedAt != nil {
		activityDate = gray(pr.UpdatedAt.Format("2006/01/02"))
	}

	fmt.Printf("%s %s %s %s %s/%s#%d - %s\n",
		activityDate,
		status,
		cyan(label),
		yellow(pr.User.GetLogin()),
		owner, repo, *pr.Number,
		*pr.Title,
	)
}
