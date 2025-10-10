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
	Label      string
	Owner      string
	Repo       string
	PR         *github.PullRequest
	UpdatedAt  time.Time
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

func fetchAndDisplayActivity(token, username string) {
	ctx := context.Background()
	client := github.NewClient(nil).WithAuthToken(token)

	// Track seen PRs to avoid duplicates
	seenPRs := make(map[string]bool)
	activities := []PRActivity{}

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

	// 6. PRs where user commented (but not already covered by other searches)
	searchQuery = fmt.Sprintf("is:pr commenter:%s state:open", username)
	activities = collectSearchResults(ctx, client, searchQuery, "💭 Commented", seenPRs, activities)

	// 7. PRs involving the user in any way (comprehensive catch-all)
	searchQuery = fmt.Sprintf("is:pr involves:%s state:open", username)
	activities = collectSearchResults(ctx, client, searchQuery, "🔗 Involved", seenPRs, activities)

	// 8. Get user's repositories and check for PRs (using non-deprecated API)
	opts := &github.RepositoryListByUserOptions{
		ListOptions: github.ListOptions{PerPage: 100},
	}
	repos, _, err := client.Repositories.ListByUser(ctx, username, opts)
	if err != nil {
		color.Red("Error fetching repositories: %v", err)
		return
	}

	for _, repo := range repos {
		prOpts := &github.PullRequestListOptions{
			State:       "open",
			ListOptions: github.ListOptions{PerPage: 100},
		}
		prs, _, err := client.PullRequests.List(ctx, username, *repo.Name, prOpts)
		if err != nil {
			continue
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
	}

	if len(activities) == 0 {
		color.Yellow("No open PR activity found")
		return
	}

	// Sort by UpdatedAt descending (newest first)
	sort.Slice(activities, func(i, j int) bool {
		return activities[i].UpdatedAt.After(activities[j].UpdatedAt)
	})

	// Display sorted activities
	for _, activity := range activities {
		displayPR(activity.Label, activity.Owner, activity.Repo, activity.PR)
	}
}

func collectSearchResults(ctx context.Context, client *github.Client, query, label string, seenPRs map[string]bool, activities []PRActivity) []PRActivity {
	opts := &github.SearchOptions{
		ListOptions: github.ListOptions{PerPage: 100},
	}

	result, resp, err := client.Search.Issues(ctx, query, opts)
	if err != nil {
		color.Red("Error searching '%s': %v", query, err)
		if resp != nil {
			color.Red("Rate limit remaining: %d", resp.Rate.Remaining)
		}
		return activities
	}

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
		}
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

