package main

import (
	"flag"
	"html/template"
	"io"
	"net/http"
	"net/url"
	"os"
	"path"
	"path/filepath"
	"strings"
)

type FileInfo struct {
	Name     string
	IsDir    bool
	FullPath string
}

type DirInfo struct {
	Path   string
	Parent string
	Files  []FileInfo
}

var (
	t       *template.Template
	rootDir string
)

const tmpl = `
		<!DOCTYPE html>
<html>
<head>
<title>File Explorer</title>
<style>
body {
    font-family: Arial, sans-serif;
    margin: 20px;
    text-align: left;
}
h1 {
    color: #333;
}
ul {
    list-style-type: none;
    padding: 0;
}
li {
    margin: 5px 0;
}
a {
    text-decoration: none;
    color: #007BFF;
}
a:hover {
    text-decoration: underline;
}
a.dir {
    font-weight: bold;
    color: #0056b3;
}
form {
    margin-top: 20px;
}
input[type="submit"] {
    background-color: #007BFF;
    color: white;
    border: none;
    padding: 5px 10px;
    cursor: pointer;
}
input[type="submit"]:hover {
    background-color: #0056b3;
}
#progress-container {
    margin-top: 10px;
    display: none;
}
progress {
    width: 300px;
    height: 20px;
}
</style>
</head>
<body>
<h1>Directory: /{{.Path}}</h1>
{{if .Path}}
<a href="/browse?path={{.Parent | urlquery }}">..</a><br>
{{end}}
<ul>
{{range .Files}}
<li>
{{if .IsDir}}
<a class="dir" href="/browse?path={{.FullPath | urlquery }}">{{.Name}}/</a>
{{else}}
<a href="/download?path={{.FullPath | urlquery }}">{{.Name}}</a>
{{end}}
</li>
{{end}}
</ul>
<form id="upload-form" action="/upload?path={{.Path | urlquery }}" method="post" enctype="multipart/form-data">
<input type="file" name="file" />
<input type="submit" value="Upload" />
</form>
<div id="progress-container">
<progress id="progress-bar" value="0" max="100"></progress>
</div>
<script>
document.getElementById('upload-form').addEventListener('submit', function(e) {
    e.preventDefault();
    var formData = new FormData(this);
    var xhr = new XMLHttpRequest();
    xhr.open('POST', this.action, true);
    xhr.upload.onprogress = function(event) {
        if (event.lengthComputable) {
            var percent = (event.loaded / event.total) * 100;
            document.getElementById('progress-bar').value = percent;
        }
    };
    xhr.onload = function() {
        if (xhr.status === 200) {
        window.location.reload()
        } else {
            alert('Upload failed: ' + xhr.status);
            document.getElementById('progress-container').style.display = 'none';
        }
    };
    xhr.onerror = function() {
        alert('Upload error');
        document.getElementById('progress-container').style.display = 'none';
    };
    document.getElementById('progress-container').style.display = 'block';
    xhr.send(formData);
});
</script>
</body>
</html>
	`

var host string

func main() {
	flag.StringVar(&rootDir, "root", ".", "root directory to serve")
	port := flag.String("port", "8080", "port to listen on")
	flag.StringVar(&host, "host", "", "allowed host patterns")
	flag.Parse()

	var err error
	rootDir, err = filepath.Abs(rootDir)
	if err != nil {
		panic(err)
	}

	t = template.Must(template.New("dir").Funcs(template.FuncMap{
		"urlquery": url.QueryEscape,
	}).Parse(tmpl))

	http.HandleFunc("/browse", browseHandler)
	http.HandleFunc("/download", downloadHandler)
	http.HandleFunc("/upload", uploadHandler)
	http.ListenAndServe(":"+*port, nil)
}

func getParts(queryPath string) []string {
	queryPath = strings.TrimPrefix(queryPath, "/")
	if queryPath == "" {
		return []string{}
	}
	return strings.Split(queryPath, "/")
}

func browseHandler(w http.ResponseWriter, r *http.Request) {
	if !strings.Contains(r.RemoteAddr, host) {
		http.Error(w, "Forbidden", http.StatusSeeOther)
		return
	}

	parts := getParts(r.URL.Query().Get("path"))
	currURLPath := strings.Join(parts, "/")
	effectivePath := filepath.Join(append([]string{rootDir}, parts...)...)
	effectivePath = filepath.Clean(effectivePath)

	sep := string(filepath.Separator)
	rootPrefix := rootDir + sep
	if effectivePath != rootDir && !strings.HasPrefix(effectivePath, rootPrefix) {
		http.Error(w, "Forbidden", http.StatusForbidden)
		return
	}

	entries, err := os.ReadDir(effectivePath)
	if err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}

	var files []FileInfo
	for _, entry := range entries {
		if entry.Name() == "." || entry.Name() == ".." {
			continue
		}
		fi := FileInfo{
			Name:     entry.Name(),
			IsDir:    entry.IsDir(),
			FullPath: path.Join(currURLPath, entry.Name()),
		}
		files = append(files, fi)
	}

	var parent string
	if len(parts) > 0 {
		parent = strings.Join(parts[:len(parts)-1], "/")
	}

	data := DirInfo{
		Path:   currURLPath,
		Parent: parent,
		Files:  files,
	}

	err = t.Execute(w, data)
	if err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
	}
}

func downloadHandler(w http.ResponseWriter, r *http.Request) {
	if !strings.Contains(r.RemoteAddr, host) {
		http.Error(w, "Forbidden", http.StatusSeeOther)
		return
	}
	parts := getParts(r.URL.Query().Get("path"))
	effectivePath := filepath.Join(append([]string{rootDir}, parts...)...)
	effectivePath = filepath.Clean(effectivePath)

	sep := string(filepath.Separator)
	rootPrefix := rootDir + sep
	if effectivePath != rootDir && !strings.HasPrefix(effectivePath, rootPrefix) {
		http.Error(w, "Forbidden", http.StatusForbidden)
		return
	}

	info, err := os.Stat(effectivePath)
	if err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}
	if info.IsDir() {
		http.Error(w, "Cannot download directory", http.StatusBadRequest)
		return
	}

	http.ServeFile(w, r, effectivePath)
}

func uploadHandler(w http.ResponseWriter, r *http.Request) {
	if !strings.Contains(r.RemoteAddr, host) {
		http.Error(w, "Forbidden", http.StatusSeeOther)
		return
	}
	if r.Method != http.MethodPost {
		http.Error(w, "Method not allowed", http.StatusMethodNotAllowed)
		return
	}

	parts := getParts(r.URL.Query().Get("path"))
	// currURLPath := strings.Join(parts, "/")
	effectivePath := filepath.Join(append([]string{rootDir}, parts...)...)
	effectivePath = filepath.Clean(effectivePath)

	sep := string(filepath.Separator)
	rootPrefix := rootDir + sep
	if effectivePath != rootDir && !strings.HasPrefix(effectivePath, rootPrefix) {
		http.Error(w, "Forbidden", http.StatusForbidden)
		return
	}

	err := r.ParseMultipartForm(32 << 20) // 32 MB max
	if err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}

	file, handler, err := r.FormFile("file")
	if err != nil {
		http.Error(w, "No file uploaded", http.StatusBadRequest)
		return
	}
	defer file.Close()

	destPath := filepath.Join(effectivePath, handler.Filename)
	out, err := os.Create(destPath)
	if err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}
	defer out.Close()

	_, err = io.Copy(out, file)
	if err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}

	w.WriteHeader(200)

	// http.Redirect(w, r, "/browse?path="+url.QueryEscape(currURLPath), http.StatusSeeOther)
}
