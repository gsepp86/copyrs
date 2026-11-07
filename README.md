# CopyRs

Un copiatore di file **leggero, gratuito e open source** scritto in Rust, ispirato ai classici copiatori di file per Windows: una finestra compatta, una coda di copia e il controllo che i file siano arrivati davvero integri.

## Se vuoi contribuire al mio lavoro ti lascio in basso il pulsante per contribuire anche con una piccola donazione simbolica visto che una app così utile gratis per sempre lo la trovi ovunque apprezzo il gesto ;-)

[![Donate via PayPal](https://img.shields.io/badge/Donate-PayPal-blue.svg?logo=paypal)](https://paypal.me/PeppeCP)

Copiatore di file minimale con verifica CRC32 (MIT).

Build: `cargo build --release` -> `target/release/copyrs.exe`
Uso da riga di comando: `copyrs.exe file1 cartella2 ...` (precarica la coda)

Integrazione Windows:
1. Copia copyrs.exe in C:\Tools\CopyRs\
2. Invia a (consigliato, funziona con selezioni multiple): Win+R -> shell:sendto
   -> crea un collegamento a copyrs.exe chiamato "CopyRs".
3. Menu contestuale (opzionale): doppio clic su windows\context-menu.reg.
   Su Windows 11 compare sotto "Mostra altre opzioni". Rimozione: remove-context-menu.reg.

Conflitti: prima di avviare, CopyRs controlla quali file esistono gia' in destinazione
(e quanti sono identici: stessa dimensione e data) e chiede una volta sola:
Sovrascrivi / Salta / Solo se piu' recenti / Rinomina / Annulla.

Primo avvio (Windows): CopyRs chiede se aggiungere "Copia con CopyRs" e "Incolla con CopyRs"
al menu contestuale di Esplora file (solo utente corrente, nessun admin). Si puo' riaprire
con "Integrazione Windows..." e rimuovere da li. NON sostituisce Ctrl+C/Ctrl+V di Windows.

Impostazioni: tema scuro (predefinito) o chiaro, e attivazione/rimozione dell'integrazione
con Windows (menu contestuale). La scelta del tema viene ricordata.
