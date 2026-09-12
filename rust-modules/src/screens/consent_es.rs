//! Spanish bodies for the consent / Privacy & data screen. The English `const`s and inline
//! literals in `consent.rs` stay exactly as they are: the tests pin their claims and the EN text
//! remains the source the audit reads. Each `*_ES` here is the translation the Spanish locale
//! draws, picked by `i18n::is_es()` in `consent.rs` - a static pick, never a build, same pattern
//! as `legal_es.rs`. The previews and identifier documents are translated faithfully, with the
//! technical tokens (Sentry/PostHog, Germany, the identifiers, the JSON field names) kept EXACT,
//! and with no em-dash in the Spanish text (a normal hyphen or comma instead).

// ---- first-run question bodies -----------------------------------------------------------------

pub(crate) const CRASH_BODY_ES: &str = "Si PlxNative se bloquea, o falla el inicio de sesion, puede enviar detalles tecnicos que ayudan a encontrar y corregir el problema. Los informes pueden incluir la senal, las direcciones de codigo, la informacion del hilo y del dispositivo, o que paso del inicio de sesion fallo y como respondio la conexion, ademas de un identificador aleatorio de informe de fallos, creado al activarlo y borrado al desactivarlo o al cerrar sesion, para que los fallos repetidos bajo un mismo identificador se cuenten una vez y no una por fallo. Nunca incluyen titulos, cuentas Plex, busquedas, nombres o direcciones de servidores, tokens, texto de subtitulos ni el identificador de analitica de uso.";

pub(crate) const PRODUCT_BODY_ES: &str = "PlxNative puede compartir que pantallas y funciones se usan y resultados generales de inicio de sesion y de reproduccion. Los informes llevan un ID de analitica aleatorio, creado al activarlo y borrado al desactivarlo o al cerrar sesion, y pueden incluir la version de la app, la version de webOS, el modelo de television y el SoC, y si el servidor seleccionado es local, remoto o retransmitido. Nunca incluyen titulos, cuentas Plex, busquedas, nombres o direcciones de servidores, tokens, texto de subtitulos ni el historial de visionado exacto.";

pub(crate) const DELETE_SCOPE_ES: &str = "Esto cierra la sesion y elimina los datos de PlxNative guardados en esta television. No borra los datos ya enviados a Plex, a tus Plex Media Servers, a Sentry o a PostHog.";

pub(crate) const SETTINGS_COPY_ES: &str = "Controla los informes opcionales, revisa exactamente que puede compartirse y gestiona los datos que PlxNative guarda en esta television.";

pub(crate) const POLICY_SUBTITLE_ES: &str = "Como gestiona PlxNative los datos locales, los servicios de Plex y los informes opcionales.";

// ---- payload previews --------------------------------------------------------------------------

pub(crate) const PREVIEW_CRASH_INTRO_ES: &str = "Fallos / Errores: que se envia realmente a Sentry en Alemania, y solo cuando los informes de errores estan activados. Los valores aleatorios y especificos de cada compilacion son marcadores; las clases fijas de abajo son valores representativos de los dominios cerrados del aviso de privacidad. No se envia nada mas. El identificador de informe de fallos es aleatorio, se crea solo cuando los informes de fallos estan activados y se muestra aqui como marcador.\n\n";

pub(crate) const CRASH_NATIVE_LABEL_ES: &str = "Informe de fallo nativo (solo cuando los informes de errores estan activados):\n";
pub(crate) const CRASH_FALLBACK_SUFFIX_ES: &str = " (solo si la captura nativa no esta disponible):\n";
pub(crate) const CRASH_PLAYBACK_LABEL_ES: &str = "\n\nError de reproduccion gestionado (solo cuando los informes de errores estan activados):\n";
pub(crate) const CRASH_SIGNIN_LABEL_ES: &str = "\n\nInforme de problema de inicio de sesion (automaticamente solo cuando los informes de errores estan activados; si no, solo al pulsar Enviar informe, y entonces sin el identificador de informe de fallos):\n";

pub(crate) const PREVIEW_USAGE_INTRO_ES: &str = "Analitica / Uso: que se envia realmente a PostHog en Alemania, y solo cuando los informes de uso estan activados, con un ID de analitica aleatorio. Los valores aleatorios y especificos de cada compilacion son marcadores; las clases fijas de abajo son valores representativos de los dominios cerrados del aviso de privacidad. No se envia nada mas. El identificador de uso es aleatorio y se crea solo cuando la analitica de uso esta activada.\n\n";

pub(crate) const USAGE_EVENTS_LABEL_ES: &str = "Eventos de uso (solo cuando los informes de uso estan activados):\n";

// ---- pushed-document subtitles -----------------------------------------------------------------

pub(crate) const ERRORS_ID_SUBTITLE_ES: &str = "El identificador aleatorio que se adjunta a los informes de fallos y errores de este inicio de sesion, y como pedir que se borren esos informes.";
pub(crate) const ANALYTICS_ID_SUBTITLE_ES: &str = "El identificador aleatorio que se adjunta a la analitica de uso de este inicio de sesion, y como pedir que se borren esos eventos.";
pub(crate) const CRASH_SUBTITLE_ES: &str = "Que se envia realmente: los campos exactos que puede llevar un informe de fallos o errores, solo cuando los informes de errores estan activados.";
pub(crate) const USAGE_SUBTITLE_ES: &str = "Que se envia realmente: los campos exactos que puede llevar un evento de analitica de uso, solo cuando los informes de uso estan activados.";

// ---- identifier documents (templates with {id}/{CONTACT_EMAIL}) --------------------------------

pub(crate) const ANALYTICS_ID_DOC_ES: &str = "TU ID DE ANALITICA\n\n{id}\n\nQUE ES\n\nUn identificador aleatorio creado en esta television cuando activaste la analitica de uso. Se adjunta a los eventos de analitica para poder contarlos como procedentes de un mismo ID de analitica: una unica alta ininterrumpida en una television. No se deriva de tu cuenta Plex, de tu television ni de nada sobre ti, y nunca se envia con los informes de fallos, que llevan un ID de informe de fallos propio.\n\nCOMO PEDIR QUE SE BORREN ESTOS EVENTOS\n\nEscribe a {CONTACT_EMAIL} e indica el identificador de arriba. Es el unico identificador que llevan estos eventos, por lo que una peticion sin el no puede asociarse a nada.\n\nCOMO TERMINA\n\nDesactivar la analitica de uso borra este identificador, y volver a activarla crea otro distinto. Cerrar sesion tambien lo elimina, y a la siguiente persona que inicie sesion se le vuelve a preguntar; lo mismo hace Borrar todos los datos locales. Los eventos ya enviados conservan el identificador antiguo, por eso conviene copiarlo antes de desactivar la analitica si piensas pedir su borrado.";

pub(crate) const NO_ANALYTICS_ID_DOC_ES: &str = "SIN ID DE ANALITICA\n\nLa analitica de uso esta desactivada, asi que esta instalacion no tiene identificador de analitica y no envia eventos de analitica.\n\nUn identificador se crea solo cuando activas la analitica de uso, y borrarlo es lo que hace desactivarla. Si antes tenias la analitica activada y quieres que se borren los eventos de ese periodo, escribe a {CONTACT_EMAIL}; ten en cuenta que el identificador que llevaban se destruyo al desactivar la analitica, por lo que ya no puede consultarse desde esta television.\n\nLos informes de fallos no usan este identificador. Llevan un ID de informe de fallos propio, que se muestra en su propia fila mientras los informes de fallos estan activados.";

pub(crate) const ERRORS_ID_DOC_ES: &str = "TU ID DE INFORME DE FALLOS\n\n{id}\n\nQUE ES\n\nUn identificador aleatorio creado en esta television cuando activaste los informes de fallos. Se adjunta a cada informe de fallos y errores para que los fallos repetidos bajo un mismo ID de informe de fallos se cuenten una vez, lo que permite distinguir un problema que afecto a mucha gente de una television que lo sufrio muchas veces. No se deriva de tu cuenta Plex, de tu television ni de nada sobre ti, y nunca se envia con la analitica de uso, que tiene un ID de analitica propio.\n\nCOMO PEDIR QUE SE BORREN ESTOS INFORMES\n\nEscribe a {CONTACT_EMAIL} e indica el identificador de arriba. Es el unico identificador que llevan estos informes, por lo que una peticion sin el no puede asociarse a nada.\n\nCOMO TERMINA\n\nDesactivar los informes de fallos borra este identificador, y volver a activarlos crea otro distinto. Cerrar sesion tambien lo elimina, y a la siguiente persona que inicie sesion se le vuelve a preguntar; lo mismo hace Borrar todos los datos locales. Los informes ya enviados conservan el identificador antiguo, por eso conviene copiarlo antes de desactivar los informes de fallos si piensas pedir su borrado.";

pub(crate) const NO_ERRORS_ID_DOC_ES: &str = "SIN ID DE INFORME DE FALLOS\n\nLos informes de fallos estan desactivados, asi que esta instalacion no tiene identificador de informe de fallos y no envia informes de fallos ni de errores.\n\nUn identificador se crea solo cuando activas los informes de fallos, y borrarlo es lo que hace desactivarlos. Si antes tenias los informes de fallos activados y quieres que se borren los informes de ese periodo, escribe a {CONTACT_EMAIL}; ten en cuenta que el identificador que llevaban se destruyo al desactivarlos, por lo que ya no puede consultarse desde esta television.";
