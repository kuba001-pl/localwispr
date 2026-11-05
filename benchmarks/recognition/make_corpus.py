#!/usr/bin/env python3
"""Author/freeze the corpus BEFORE decoding. No recognizer outputs are inputs."""
import hashlib
import json
from pathlib import Path

OUT = Path(__file__).resolve().parent
NAMES = [
    'Qdrant', 'Anthropic', 'Cerebras', 'Vercel', 'Supabase', 'ElevenLabs',
    'Hugging Face', 'OpenTelemetry', 'ClickHouse', 'Tailscale', 'Linear',
    'Notion', 'Cursor', 'Perplexity', 'PostgreSQL', 'Kubernetes', 'Grafana',
    'Prometheus', 'Datadog', 'LangChain', 'Weaviate', 'Pinecone', 'Snowflake',
    'Databricks', 'Cloudflare', 'GitLab', 'GitHub', 'DeepSeek', 'Mistral',
    'Modal', 'Railway', 'Fly.io', 'Replicate', 'Together AI', 'Groq', 'Svelte',
    'SQLite', 'DuckDB', 'NixOS', 'Temporal', 'Elastic', 'Sentry',
]
# Deliberately varied surrounding sentences. Product names are inserted as
# literal source spellings, without phonetic rewriting or output corrections.
EN = [
    'Before lunch, please check the backup from {n} and tell me whether it finished.',
    'The invoice mentions {n}, but the purchase order still has the old address.',
    'I asked the new colleague to compare {n} with our current system.',
    'After the meeting, open {n} and export only the small example.',
    'Our staging account in {n} needs a separate password before Friday.',
    'Do not upload customer recordings to {n}; this is just a local demonstration.',
    'Could you explain why {n} appears twice in the architecture drawing?',
    'Yesterday we moved the trial to {n}, and today the dashboard is empty.',
    'Please write {n} in the agenda beside the migration discussion.',
    'The security team approved {n} after checking the access policy.',
    'Move the issue into {n} after you attach the screenshot.',
    'I saved the meeting notes in {n} under the autumn planning folder.',
    'She uses {n} to review a small change before taking the train home.',
    'The research memo lists {n} alongside three independent search tools.',
    'When the build fails, verify the connection to {n} before restarting anything.',
    'Two workers in {n} stopped responding during the evening maintenance window.',
    'The chart in {n} shows a brief spike after the release.',
    'Ask whether {n} can retain those metrics for another week.',
    'The alert arrived from {n} while I was making coffee.',
    'For the workshop, create a tiny example using {n} and an empty document.',
    'Our vector search experiment uses {n} for the temporary collection.',
    'We should estimate the cost of {n} before increasing the index size.',
    'The analyst left the query running in {n} overnight by mistake.',
    'Send the notebook from {n} to the reviewer after removing the sample keys.',
    'The domain points through {n}, although the application runs somewhere else.',
    'Please reopen the merge request in {n} when the checks turn green.',
    'There is a discussion about {n} in the engineering handbook.',
    'The evaluation includes {n}, but no decision has been made yet.',
    'I wrote a short note about {n} on the second page of the proposal.',
    'Our batch job on {n} should finish before the first bus arrives.',
    'Check the deployment on {n} after you change the environment variable.',
    'The demonstration runs on {n} with a single small machine.',
    'The image example from {n} should use a public test file.',
    'Can we reserve a small budget for {n} during the next sprint?',
    'The latency measurement for {n} excludes network capture and file loading.',
    'Build the settings panel with {n}, then ask someone to check the keyboard shortcuts.',
    'The offline cache uses {n} because the entire database fits in one file.',
    'Try the same analytical query in {n} without changing the input rows.',
    'The laptop running {n} needs a reboot after the package update.',
    'That background workflow in {n} must survive a worker restart.',
    'We put the search index in {n} and kept the original documents separately.',
    'Please attach the stack trace from {n} to the incident report.',
]
PL = [
    'Przed obiadem sprawdź kopię z {n} i zapisz wynik w zeszycie.',
    'Na fakturze widnieje {n}, ale numer zamówienia jest niepoprawny.',
    'Nowa koleżanka porówna {n} z naszym obecnym rozwiązaniem w środę.',
    'Po spotkaniu otwórz {n} i pobierz mały przykład do prezentacji.',
    'Nasze konto testowe w {n} wymaga osobnego hasła do piątku.',
    'Nie wysyłaj nagrań klientów do {n}, przygotujemy publiczny przykład.',
    'Wyjaśnij proszę, dlaczego {n} pojawia się dwa razy na diagramie.',
    'Wczoraj przenieśliśmy próbę do {n}, a dziś wykres jest pusty.',
    'Dopisz {n} do planu rozmowy o przeniesieniu danych.',
    'Zespół bezpieczeństwa zaakceptował {n} po sprawdzeniu uprawnień.',
    'Przenieś zadanie do {n} dopiero po dodaniu zrzutu ekranu.',
    'Notatki ze spotkania zapisałam w {n} w osobnym katalogu.',
    'Kolega używa {n} do przeglądu zmian przed wyjazdem do domu.',
    'Raport badawczy wymienia {n} obok kilku innych narzędzi.',
    'Gdy test zawiedzie, sprawdź połączenie z {n}, zanim uruchomisz usługę ponownie.',
    'Dwa procesy w {n} przestały odpowiadać podczas nocnej konserwacji.',
    'Na wykresie w {n} widać krótki skok po ostatnim wydaniu.',
    'Zapytaj, czy {n} przechowa te pomiary przez kolejny tydzień.',
    'Powiadomienie z {n} przyszło, gdy przygotowywałem herbatę.',
    'Na warsztaty przygotuj przykład z {n} i pustym dokumentem.',
    'Eksperyment z wyszukiwaniem używa {n} do tymczasowej kolekcji.',
    'Oszacuj koszt {n}, zanim zwiększymy rozmiar indeksu.',
    'Analityczka zostawiła zapytanie w {n} na całą noc.',
    'Wyślij notatnik z {n} do recenzenta po usunięciu kluczy.',
    'Domena kieruje przez {n}, chociaż aplikacja działa na innym serwerze.',
    'Otwórz ponownie wniosek w {n}, kiedy wszystkie testy przejdą.',
    'W podręczniku zespołu jest dyskusja o {n} i uprawnieniach.',
    'Ocena obejmuje {n}, ale nie podjęliśmy jeszcze decyzji.',
    'Krótka uwaga o {n} znajduje się na drugiej stronie propozycji.',
    'Zadanie na {n} powinno skończyć się przed porannym autobusem.',
    'Sprawdź wdrożenie w {n} po zmianie tej zmiennej środowiskowej.',
    'Demonstracja działa na {n} na jednej małej maszynie.',
    'Przykład obrazu z {n} powinien korzystać z publicznego pliku.',
    'Czy możemy przeznaczyć mały budżet na {n} w następnym miesiącu?',
    'Pomiar opóźnienia dla {n} pomija nagrywanie i ładowanie pliku.',
    'Zbuduj panel ustawień w {n}, potem sprawdź skróty klawiaturowe.',
    'Lokalna pamięć używa {n}, ponieważ baza mieści się w jednym pliku.',
    'Uruchom to zapytanie w {n} bez zmiany wierszy wejściowych.',
    'Laptop z {n} wymaga ponownego uruchomienia po aktualizacji.',
    'Proces w tle w {n} musi przetrwać restart wykonawcy.',
    'Indeks umieściliśmy w {n}, a oryginalne dokumenty zachowaliśmy osobno.',
    'Dołącz ślad błędu z {n} do raportu o awarii.',
]
MIX = [
    'Sprawdź {n} before the meeting, a potem wróć do raportu.',
    'The invoice for {n} jest już gotowa, tylko adres wymaga poprawki.',
    'Porównaj {n} with our old setup i zapisz trzy różnice.',
    'Open {n} po przerwie i pokaż mały przykład zespołowi.',
    'Hasło do {n} expires on Friday, więc zmień je dzisiaj.',
    'Keep {n} on the test account, bo dane są wyłącznie przykładowe.',
    'Dlaczego {n} appears twice tutaj, skoro potrzebujemy jednej usługi?',
    'Yesterday we tried {n}, ale dzisiaj panel niczego nie pokazuje.',
    'Dodaj {n} to the agenda i wyślij ją przed obiadem.',
    'Security approved {n}, teraz czekamy na zgodę administratora.',
    'Move the issue to {n}, a potem dołącz plik z opisem.',
    'Notatki są w {n}, please read the last paragraph przed rozmową.',
    'Review this in {n}, zanim wsiądziesz do pociągu do Krakowa.',
    'Raport mentions {n}, ale nie porównuje wyników wyszukiwania.',
    'Check {n} first, potem zrestartuj proces, jeśli nadal nie działa.',
    'Two workers in {n} zniknęły podczas wieczornej aktualizacji.',
    'Wykres w {n} shows a spike, ale tylko przez dwie sekundy.',
    'Ask about {n} retention, bo potrzebujemy danych z całego tygodnia.',
    'Alert from {n} przyszedł w trakcie przerwy na kawę.',
    'Create a small {n} example, bez prawdziwych danych klientów.',
    'Testujemy {n} for vector search i jutro porównamy wyniki.',
    'Estimate the {n} cost, zanim dodamy następny milion rekordów.',
    'Query in {n} nadal działa, please stop it before lunch.',
    'Wyślij notebook z {n}, but remove the secret values najpierw.',
    'Traffic goes through {n}, a serwer stoi w innym regionie.',
    'Reopen the request in {n}, kiedy testy będą zielone.',
    'Handbook opisuje {n}, read the permissions section przed zmianą.',
    'We included {n} w ocenie, ale decyzja należy do zespołu.',
    'Notatka o {n} is on page two, zaraz pod tabelą kosztów.',
    'The {n} job kończy się rano, więc zostaw komputer włączony.',
    'Deployment on {n} wymaga nowej zmiennej, please check the spelling.',
    'Demo runs on {n}, ale tylko na jednej małej maszynie.',
    'Image from {n} jest przykładowy, keep the public file w katalogu.',
    'Reserve a budget for {n}, a szczegóły omówimy w poniedziałek.',
    'Latency for {n} excludes capture, czyli mierzymy samo przetwarzanie.',
    'Panel uses {n}, teraz sprawdź działanie klawisza tabulacji.',
    'Cache in {n} jest lokalny, no network connection is needed.',
    'Try {n} with this query, nie zmieniaj kolejności wierszy.',
    'Laptop with {n} needs a restart, zanim zaczniemy prezentację.',
    'Workflow in {n} przetrwa restart, please verify that tomorrow.',
    'Search uses {n}, ale dokumenty zapisujemy w osobnym miejscu.',
    'Trace from {n} jest w załączniku, read it before the call.',
]
# Ordinary words / near-homophones, each paired with a misleading brand context.
# Case-only brand ambiguity is measured separately from lexical substitution.
CONTROLS = [
 ('Notion', 'notion', 'I had a strange notion about the shape of that cloud.', 'Mam dziwne pojęcie o chmurach, a słowo notion jest zwykłym rzeczownikiem.', 'Mam taką myśl, a strange notion about the clouds, i to wszystko.'),
 ('Cursor', 'cursor', 'Move the cursor slowly to the left edge of the empty box.', 'Przesuń kursor do lewego brzegu pustego pola i zaczekaj.', 'Przesuń cursor to the left, a potem kliknij puste pole.'),
 ('Linear', 'linear', 'The graph has a linear slope throughout the quiet part of the test.', 'Zależność jest liniowa, bez żadnej nowej aplikacji na komputerze.', 'To jest linear growth, czyli zwykły prosty wzrost wartości.'),
 ('Anthropic', 'anthropic', 'The anthropic principle appears in the final chapter about cosmology.', 'Zasada antropiczna pojawia się w rozdziale o kosmologii.', 'Omawiamy the anthropic principle, a potem wracamy do fizyki.'),
 ('Perplexity', 'perplexity', 'Her perplexity was obvious when she opened the empty envelope.', 'Jej zdziwienie było widoczne po otwarciu pustej koperty.', 'Jej perplexity was obvious, ale nikt nie znał odpowiedzi.'),
 ('Cerebras', 'cerebral', 'The doctor described a cerebral artery in the drawing on the wall.', 'Lekarz opisał tętnicę mózgową na rysunku wiszącym na ścianie.', 'Lekarz powiedział cerebral artery i wskazał miejsce na rysunku.'),
 ('Qdrant', 'quadrant', 'Shade the upper right quadrant of the circle with a soft pencil.', 'Zamaluj prawą górną ćwiartkę koła miękkim ołówkiem.', 'Zamaluj the upper right quadrant i odłóż ołówek na stół.'),
 ('Vercel', 'verse', 'Read the second verse aloud before turning to the next page.', 'Przeczytaj drugi wers głośno, zanim przewrócisz stronę książki.', 'Read the second verse, a potem przewróć stronę powoli.'),
 ('Supabase', 'superb bass', 'That old speaker has superb bass even at a very low volume.', 'Ten stary głośnik ma świetny bas nawet przy małej głośności.', 'Ten speaker has superb bass, chociaż jest bardzo mały.'),
 ('ElevenLabs', 'eleven labs', 'The university has eleven labs spread across three older buildings.', 'Uniwersytet ma jedenaście laboratoriów w trzech starych budynkach.', 'Mamy eleven labs na uczelni i trzy sale wykładowe.'),
 ('Hugging Face', 'hugging', 'The child was hugging a soft toy while waiting beside the window.', 'Dziecko przytulało miękką zabawkę, czekając przy oknie.', 'Dziecko was hugging a toy, zanim mama otworzyła drzwi.'),
 ('OpenTelemetry', 'telemetry', 'The balloon sent open telemetry readings during the whole flight.', 'Balon przesyłał dane telemetryczne podczas całego krótkiego lotu.', 'Balon wysyła open telemetry readings i ląduje za lasem.'),
 ('ClickHouse', 'click', 'Click the house shaped icon once and wait for the map to appear.', 'Kliknij ikonę domku raz i poczekaj na pojawienie się mapy.', 'Click the house icon, potem poczekaj chwilę na mapę.'),
 ('Tailscale', 'tail', 'The fish has a broad tail and one loose scale near its fin.', 'Ryba ma szeroki ogon i jedną luźną łuskę przy płetwie.', 'Ryba has a broad tail, a jedna scale leży na piasku.'),
 ('Pinecone', 'pinecone', 'A dry pinecone rolled under the garden bench after the wind picked up.', 'Sucha szyszka potoczyła się pod ławkę w ogrodzie.', 'Ta pinecone rolled under the bench, zanim zaczęło padać.'),
 ('Snowflake', 'snowflake', 'A single snowflake melted on the warm sleeve of his coat.', 'Jeden płatek śniegu stopniał na ciepłym rękawie kurtki.', 'Mały snowflake melted on my sleeve, kiedy zamknąłem drzwi.'),
 ('Mistral', 'mistral', 'A cold mistral blew through the valley before sunrise yesterday.', 'Zimny mistral wiał przez dolinę przed wschodem słońca.', 'Dzisiaj a cold mistral blows through the valley, więc zamknij okno.'),
 ('Modal', 'modal', 'The grammar exercise asks us to underline each modal verb in blue.', 'Ćwiczenie z gramatyki każe podkreślić każdy czasownik modalny.', 'Podkreśl każdy modal verb na niebiesko, potem sprawdź odpowiedzi.'),
 ('Railway', 'railway', 'The railway bridge crosses a narrow river near the village school.', 'Most kolejowy przecina wąską rzekę obok wiejskiej szkoły.', 'The railway bridge jest stary, ale pociągi nadal po nim jeżdżą.'),
 ('Replicate', 'replicate', 'Please replicate the paper pattern on a larger sheet for the children.', 'Odtwórz papierowy wzór na większej kartce dla dzieci.', 'Please replicate ten wzór na większej kartce przed lekcją.'),
 ('Together AI', 'together', 'We worked together all morning to repair the wooden garden gate.', 'Pracowaliśmy razem przez cały ranek, naprawiając drewnianą bramę.', 'Worked together all morning, a potem zjedliśmy wspólny obiad.'),
 ('Groq', 'grok', 'It took me a while to grok the meaning of that unusual joke.', 'Zrozumienie znaczenia tego nietypowego żartu zajęło mi chwilę.', 'Nie mogę grok that joke, wyjaśnij mi go jeszcze raz.'),
 ('Svelte', 'svelte', 'The illustrator drew a svelte figure wearing a long green coat.', 'Ilustratorka narysowała smukłą postać w długim zielonym płaszczu.', 'Narysuj a svelte figure, ale zostaw płaszcz bez koloru.'),
 ('Temporal', 'temporal', 'The study found a temporal pattern in the migration of those birds.', 'Badanie wykazało czasowy wzorzec w migracji tych ptaków.', 'To jest temporal pattern w migracji, a nie wynik pomiaru odległości.'),
 ('Elastic', 'elastic', 'The elastic band snapped when I stretched it around the thick book.', 'Gumka pękła, gdy rozciągnąłem ją wokół grubej książki.', 'Ta elastic band snapped, zanim udało mi się zamknąć pudełko.'),
 ('Sentry', 'sentry', 'A tired sentry stood beside the castle gate until the bell rang.', 'Zmęczony wartownik stał przy bramie zamku aż do dzwonu.', 'The sentry stoi przy bramie, a reszta załogi odpoczywa.'),
 ('Datadog', 'dog', 'Our dog carried a blue ball into the kitchen during breakfast.', 'Nasz pies przyniósł niebieską piłkę do kuchni podczas śniadania.', 'Our dog przyniósł piłkę, please leave it beside the door.'),
 ('Cloudflare', 'flare', 'A bright flare lit the cloudy sky above the boat for a moment.', 'Jasna raca na chwilę oświetliła pochmurne niebo nad łodzią.', 'A bright flare rozświetliła niebo, potem znowu było ciemno.'),
]
NEUTRAL_EN = [
 'Please leave the small brown parcel beside the kitchen door.',
 'Do not delete the draft before Maria has checked the final paragraph.',
 'There are seven empty cups on the shelf above the sink.',
 'Tomorrow morning I will take the early train to the coast.',
 'The little red bicycle belongs to the child across the street.',
 'We should buy fresh bread and two apples on the way home.',
 'Keep the original numbers exactly as they appear in the table.',
 'I never said that the meeting had been cancelled for everyone.',
 'Turn down the music before you answer the telephone in the hall.',
 'The next page contains a short poem about an old wooden boat.',
 'She folded the letter carefully and put it inside the blue envelope.',
 'We need another blanket because the bedroom window is still open.',
 'Please measure the width twice before cutting the narrow piece of wood.',
 'The bus stops behind the library every twenty minutes after noon.',
 'I would rather wait here until the heavy rain has stopped.',
 'Nobody knows where the spare key was left after the party.',
 'Add a little water to the pot and stir the soup slowly.',
 'His brother sent a postcard from a village near the mountain lake.',
 'Take the second path on the left after passing the stone wall.',
 'We cannot approve the change until the missing page has arrived.',
]
NEUTRAL_PL = [
 'Proszę zostawić małą brązową paczkę przy drzwiach do kuchni.',
 'Nie usuwaj szkicu, zanim Maria sprawdzi ostatni akapit dokumentu.',
 'Na półce nad zlewem stoi siedem pustych kubków po herbacie.',
 'Jutro rano pojadę wczesnym pociągiem nad spokojne morze.',
 'Mały czerwony rower należy do dziecka z drugiej strony ulicy.',
 'Kupmy świeży chleb i dwa jabłka po drodze do domu.',
 'Zachowaj oryginalne liczby dokładnie w takiej postaci jak w tabeli.',
 'Nigdy nie mówiłam, że spotkanie zostało odwołane dla wszystkich.',
 'Ścisz muzykę, zanim odbierzesz telefon stojący w przedpokoju.',
 'Na następnej stronie jest krótki wiersz o starej drewnianej łodzi.',
 'Złożyła list ostrożnie i włożyła go do niebieskiej koperty.',
 'Potrzebujemy drugiego koca, bo okno w sypialni nadal jest otwarte.',
 'Zmierz szerokość dwa razy, zanim przytniesz wąski kawałek drewna.',
 'Autobus zatrzymuje się za biblioteką co dwadzieścia minut po południu.',
 'Wolę poczekać tutaj, aż przestanie padać ten intensywny deszcz.',
 'Nikt nie wie, gdzie został zapasowy klucz po wczorajszym przyjęciu.',
 'Dolej trochę wody do garnka i powoli zamieszaj gorącą zupę.',
 'Jej brat wysłał pocztówkę ze wsi położonej nad górskim jeziorem.',
 'Wybierz drugą ścieżkę w lewo po minięciu kamiennego muru.',
 'Nie możemy zatwierdzić zmiany, dopóki nie dotrze brakująca strona.',
]
NEUTRAL_MIX = [
 'Zostaw tę paczkę by the kitchen door, potem możesz wracać.',
 'Do not delete szkicu, Maria musi jeszcze sprawdzić ostatni akapit.',
 'Na półce stoi seven empty cups, proszę je umyć przed obiadem.',
 'Tomorrow morning jadę pociągiem nad morze, więc wstanę wcześniej.',
 'Ten red bicycle należy do sąsiada z drugiej strony ulicy.',
 'Kup fresh bread i dwa jabłka, kiedy będziesz wracać do domu.',
 'Keep the original numbers, nie zaokrąglaj żadnej wartości w tabeli.',
 'Nigdy nie mówiłam the meeting is cancelled, tylko zmieniamy godzinę.',
 'Turn down the music, bo telefon w przedpokoju właśnie dzwoni.',
 'Na następnej stronie read the short poem i podkreśl ostatni wers.',
 'Fold the letter carefully, potem włóż go do niebieskiej koperty.',
 'Potrzebujemy another blanket, bo w sypialni jest za zimno.',
 'Measure the width twice, zanim przetniesz tę drewnianą listwę.',
 'Autobus stops behind the library, a potem jedzie prosto do centrum.',
 'Wolę wait here until the rain stops, nie mam parasola.',
 'Nobody knows gdzie jest klucz, więc sprawdź kieszenie kurtki.',
 'Dolej a little water do garnka i powoli zamieszaj zupę.',
 'Brat sent a postcard z małej wsi nad górskim jeziorem.',
 'Take the second path on the left, zaraz za kamiennym murem.',
 'Nie zatwierdzimy zmiany until the missing page arrives, to konieczne.',
]

def digest(s):
    return hashlib.sha256(s.encode()).hexdigest()

def main():
    dest = OUT / 'corpus.jsonl'
    if dest.exists():
        raise SystemExit('Already frozen; refusing to overwrite corpus.jsonl')
    rows = []
    for i, name in enumerate(NAMES):
        for lang, templates in [('en', EN), ('pl', PL), ('mixed', MIX)]:
            rows.append(dict(id=f'name-{i:02}-{lang}', category='name', language=lang,
                             reference=templates[i].format(n=name), targets=[name],
                             ordinary=[], decoys=[]))
    for i, (brand, word, en, pl, mixed) in enumerate(CONTROLS):
        for lang, text in [('en', en), ('pl', pl), ('mixed', mixed)]:
            # Ordinary anchors can be absent in Polish translations; only actual
            # source occurrences form the denominator for anchor preservation.
            anchors = [word] if word.casefold() in text.casefold() else []
            rows.append(dict(id=f'control-{i:02}-{lang}', category='ordinary_control', language=lang,
                             reference=text, targets=[], ordinary=anchors, decoys=[brand]))
    for lang, sentences in [('en', NEUTRAL_EN), ('pl', NEUTRAL_PL), ('mixed', NEUTRAL_MIX)]:
        for i, text in enumerate(sentences):
            rows.append(dict(id=f'neutral-{i:02}-{lang}', category='name_absent', language=lang,
                             reference=text, targets=[], ordinary=[], decoys=[]))
    assert len(rows) == 270
    assert len({r['reference'].casefold() for r in rows}) == 270
    # Stratify before seeing any outputs; SHA ordering makes assignment independent
    # of author ordering. Held-out references never influence strategy selection.
    for category in ['name', 'ordinary_control', 'name_absent']:
        for lang in ['en', 'pl', 'mixed']:
            group = sorted([r for r in rows if r['category']==category and r['language']==lang],
                           key=lambda r:digest('split-v1:'+r['id']))
            for j, r in enumerate(group):
                r['split'] = 'dev' if j % 3 == 0 else 'heldout'
                r['balanced_subset'] = j < (12 if category=='name' else 9 if category=='ordinary_control' else 5)
                r['repeat_subset'] = j < (2 if category=='name' else 3 if category=='ordinary_control' else 3)
    for i, r in enumerate(rows):
        lang = r['language']
        voices = ['en-us', 'en-gb', 'en-sc', 'en-us+f3'] if lang=='en' else ['pl', 'pl+f3', 'pl+m3']
        r.update(voice=voices[i % len(voices)], speed=[135,160,185][(i//3) % 3],
                 languages=['en'] if lang=='en' else ['pl','en'], synthetic=True)
        terms = r['targets'] or r['decoys']
        if not terms:
            terms = [NAMES[(i+5)%len(NAMES)], NAMES[(i+19)%len(NAMES)]]
        # These are fabricated document excerpts, not oracle transcriptions.
        prose = [
            'The team reviewed the plan yesterday. We need a clear summary before the next meeting. ',
            'Lista tematów jest gotowa. Proszę zapisać wnioski i sprawdzić szczegóły po przerwie. ',
            'Notes from last week: keep the old example, then compare the results after lunch. ',
        ][i%3]
        if i % 4 == 0:
            prose *= 10
        r['context_before'] = 'Project notes: '+', '.join(terms)+'. '+prose
        r['context_after'] = ' Follow up: '+', '.join(terms)+'.'
        distractors = [NAMES[(i+11)%42], NAMES[(i+23)%42], NAMES[(i+31)%42]]
        distractors = [n for n in distractors if n not in r['targets']]
        if r['decoys']:
            distractors = r['decoys'] + distractors
        r['distractor_before'] = 'Approved migration vocabulary: '+', '.join(distractors)+'. '+prose
        r['distractor_after'] = ' Keep '+', '.join(distractors)+' in the final notes.'
        r['long_hint'] = i % 13 == 0
    # Non-speech controls are additional fixtures, never counted as sentences.
    for i, kind in enumerate(['silence','white_quiet','white_loud','pink','tone','hum','clicks','fade_noise']):
        rows.append(dict(id='acoustic-'+kind, category='acoustic', language='none', reference='',
                         targets=[], ordinary=[], decoys=['Notion','Qdrant','Cursor'], split='dev' if i%3==0 else 'heldout',
                         balanced_subset=True, repeat_subset=False, synthetic=True, noise=kind,
                         languages=['pl','en'], context_before='Notion Qdrant Cursor Cerebras', context_after='',
                         distractor_before='Anthropic Linear Vercel Supabase', distractor_after='', long_hint=False))
    content = ''.join(json.dumps(r,ensure_ascii=False,sort_keys=True)+'\n' for r in rows)
    dest.write_text(content)
    plan = {
        'version':1, 'unique_sentences':270, 'acoustic_fixtures':8, 'freeze_sha256':digest(content),
        'split_rule':'SHA256(split-v1:id), every third row within category/language is dev',
        'primary':'Fast all fixtures: off / source relevant / source distractor',
        'secondary':'Balanced fixed stratified subset: same three conditions',
        'strategies_dev_only':['source','case16','prose320','short12'],
        'selection':'Minimize summed dev word errors across relevant+distractor, then minimize lexical false brand clips, then prefer source. No heldout strategy tests.',
        'repeats':'Fixed 24 speech sentences, same three Fast conditions once again, excluded from unique/micro aggregate totals',
        'long_manual':'Every thirteenth speech fixture: 280 repetitions of ordinary vocabulary then a fixed manual suffix. Identical across context conditions.',
        'metrics':['micro normalized WER/CER','literal and case-insensitive target-token occurrence accuracy',
                   'decoy lexical false brand versus case-only false brand','all absent-name insertions',
                   'ordinary anchor preservation','paired per-sentence WER changes','latency p50 p95 excluding capture, synthesis and file read',
                   'non-speech hallucinations','failure buckets by language/category/voice/speed/long_hint'],
        'limitations':'Synthetic eSpeak only; 270 unique authored sentences with related templates, not natural dictation or population accuracy. Exact product spellings can be acoustically ambiguous.',
    }
    (OUT/'plan.json').write_text(json.dumps(plan,ensure_ascii=False,indent=2)+'\n')
    print(json.dumps(plan,ensure_ascii=False,indent=2))

if __name__=='__main__': main()
