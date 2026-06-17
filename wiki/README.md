# Eva-CLI GitHub Wiki Source

This directory keeps a versioned source copy of the GitHub Wiki pages.

GitHub Wiki content is stored in a separate git repository named
`Eva-CLI.wiki.git`. The wiki remote may not exist until the Wiki is initialized
from the GitHub web UI or by a successful first push.

## Page Files

- `Home.md`
- `_Sidebar.md`
- `Architecture-Overview.md`
- `Runtime-and-Scheduling.md`
- `Adapters-and-Capabilities.md`
- `Memory,-Knowledge,-and-Discovery.md`
- `Configuration-and-Localization.md`
- `Roadmap-and-Open-Risks.md`
- `Contributor-Guide.md`

## Publish

After the GitHub Wiki remote is available, publish from this directory:

```powershell
git clone git@github.com:Yetmos/Eva-CLI.wiki.git ..\Eva-CLI.wiki
Copy-Item -Path .\wiki\Home.md, .\wiki\_Sidebar.md, .\wiki\Architecture-Overview.md, .\wiki\Runtime-and-Scheduling.md, .\wiki\Adapters-and-Capabilities.md, '.\wiki\Memory,-Knowledge,-and-Discovery.md', .\wiki\Configuration-and-Localization.md, .\wiki\Roadmap-and-Open-Risks.md, .\wiki\Contributor-Guide.md -Destination ..\Eva-CLI.wiki -Force
git -C ..\Eva-CLI.wiki add .
git -C ..\Eva-CLI.wiki commit -m "Improve architecture wiki"
git -C ..\Eva-CLI.wiki push origin master
```

If the wiki repository does not clone yet, open the repository Wiki tab on
GitHub, create the first Home page once, then rerun the commands above.
