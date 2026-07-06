# querybench

Ett litet fristående dev-verktyg för att köra SQL mot **vilken MSSQL-databas som helst**
lokalt, med syntax highlighting, schema-medveten autocomplete och typfärgade resultat.

## Kör

```bash
cd querybench
npm install      # första gången
npm start
```

Servern startar på http://localhost:5055 och försöker öppna webbläsaren automatiskt.
Sätt `PORT` för att byta port: `PORT=6060 npm start`.

Första gången möts du av en "Kom igång"-vy där du lägger till din första databas eller server.

## Modell: databaser & servrar

Det du jobbar med är **databaser** — sidopanelen visar en sökbar lista med alla dina
databaser (bara namn; vid namnkrock visas källan, t.ex. `batman (Local dev)`). En
**server** är bara ett bekvämt sätt att lägga till många databaser på en gång och dela
inloggning.

Öppna config-fönstret med **⚙** uppe till vänster för att lägga till, redigera och ta bort:

- **+ Databas** — en connection till en enskild databas (inkludera `Database=…` i strängen).
  Det vanliga när du bara bryr dig om en databas.
- **+ Server** — en connection string till en instans; vi listar dess databaser så du kan
  bocka i vilka du vill ha, eller välja **Alla** (då dyker nya databaser upp automatiskt).
- **✎ / 🗑** på varje rad för att redigera (namn / connection string) eller ta bort.

Connection strings anges i ADO.NET-format, t.ex.:
```
Server=localhost,1433;User Id=sa;Password=…;TrustServerCertificate=True;Encrypt=False
```

Allt sparas **server-side** i din användarmapp — lösenord når aldrig webbläsaren, som
bara ser namn och databaslistan. Anslutningen testas innan den sparas. Configfilen ligger i
OS-standardplatsen:

| OS | Sökväg |
| --- | --- |
| Linux | `$XDG_CONFIG_HOME/querybench/config.json` (default `~/.config/querybench/config.json`) |
| macOS | `~/Library/Application Support/querybench/config.json` |
| Windows | `%APPDATA%\querybench\config.json` |

En gammal `sources.json`/`connections.json` i projektmappen migreras automatiskt dit
första gången du kör.

Windows-autentisering (Integrated Security) stöds inte på Linux/WSL; använd en
SQL-inloggning. Vill du förifylla en default utan UI: sätt env-varen
`QUERYBENCH_CONNECTION` till en connection string innan start.

## Flera motorer

Backend har ett engine-lager (en adapter per motor). Idag finns **MSSQL**; **Postgres**
(server-baserad) och **SQLite** (serverlös — bara en fil) kan läggas till som nya adaptrar
utan att modellen, endpoints eller UI:t ändras.

## Hur det fungerar

- **Backend** (`server.js`): Express + `mssql`. Endpoints:
  - `GET /api/databases` — den platta databaslistan för sidopanelen
  - `GET /api/sources`, `POST/PATCH/DELETE /api/servers`, `POST /api/servers/preview`,
    `POST/PATCH/DELETE /api/databases` — hantera källor
  - `GET /api/schema?dbId=…` — tabeller + kolumner + radantal
  - `POST /api/query` — `{ dbId, sql }` → `{ columns, rows, rowsAffected, elapsedMs, error }`
- **Frontend** (`src/main.js`): CodeMirror 6 med `@codemirror/lang-sql`.
  Vite körs i middleware-läge inuti Express, så ett kommando startar allt.

## Användning

- Välj databas i sidopanelen (sökbar). Klick på en tabell kör `SELECT TOP 100 * FROM [schema].[table]`.
- Skriv SQL i editorn och kör med **Ctrl+Enter** eller **Run query**-knappen.
- Klick på en cell öppnar en detaljpanel till höger. JSON visas pretty-printat,
  highlightat och ihopfällbart, med kopiera-knapp. Esc stänger.
- Resultatceller är typfärgade (sträng, UUID, datum, tal, bool, NULL).
- Dra i kanterna mellan panelerna för att ändra storlek. Storlekarna sparas i localStorage.
- Alla queries tillåts (läs + skriv + DDL) — inget skyddsnät. Kör bara mot databaser
  du får ändra i.
