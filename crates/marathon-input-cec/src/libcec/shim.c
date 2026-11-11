/*
 * A small, version-independent C interface over libcec's C API, for
 * marathon-input-cec (see ../libcec/mod.rs).
 *
 * libcec's configuration struct changes size and layout with every major
 * version, so declaring it by hand in Rust would tie Marathon to one
 * version. Compiled against the installed headers, this file always gets
 * it right, and the Rust side only ever sees the plain types below.
 *
 * Politeness (PLAN.md M7): the configuration never wakes or powers off
 * the TV, and only claims "active source" when asked to (claim_on_start).
 *
 * libcec calls the callbacks on its own thread. They only forward to the
 * Rust callbacks, which must not block.
 */

#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include <cecc.h>

typedef void (*marathon_cec_key_fn)(void *user, int keycode, unsigned int duration);
typedef void (*marathon_cec_source_fn)(void *user, int activated);
typedef void (*marathon_cec_command_fn)(void *user, int initiator, int destination, int opcode);
typedef void (*marathon_cec_alert_fn)(void *user, int alert);
typedef void (*marathon_cec_log_fn)(void *user, int level, const char *message);

/* Must match `Callbacks` in mod.rs. */
struct marathon_cec_callbacks {
    marathon_cec_key_fn key;
    marathon_cec_source_fn source;
    marathon_cec_command_fn command;
    marathon_cec_alert_fn alert;
    marathon_cec_log_fn log;
    void *user;
};

/* Alert codes passed to the Rust alert callback. */
#define MARATHON_CEC_ALERT_CONNECTION_LOST 1
#define MARATHON_CEC_ALERT_PERMISSION_ERROR 2
#define MARATHON_CEC_ALERT_PORT_BUSY 3
#define MARATHON_CEC_ALERT_OTHER 4

struct marathon_cec {
    libcec_connection_t connection;
    ICECCallbacks callbacks;
    libcec_configuration config;
    struct marathon_cec_callbacks rust;
};

static void on_log(void *param, const cec_log_message *message)
{
    struct marathon_cec *cec = param;
    cec->rust.log(cec->rust.user, (int)message->level, message->message);
}

static void on_key(void *param, const cec_keypress *key)
{
    struct marathon_cec *cec = param;
    cec->rust.key(cec->rust.user, (int)key->keycode, key->duration);
}

static void on_command(void *param, const cec_command *command)
{
    struct marathon_cec *cec = param;
    if (command->opcode_set) {
        cec->rust.command(cec->rust.user, (int)command->initiator, (int)command->destination,
                          (int)command->opcode);
    }
}

static void on_alert(void *param, const libcec_alert alert, const libcec_parameter data)
{
    struct marathon_cec *cec = param;
    int code;
    (void)data;
    switch (alert) {
    case CEC_ALERT_CONNECTION_LOST:
        code = MARATHON_CEC_ALERT_CONNECTION_LOST;
        break;
    case CEC_ALERT_PERMISSION_ERROR:
        code = MARATHON_CEC_ALERT_PERMISSION_ERROR;
        break;
    case CEC_ALERT_PORT_BUSY:
        code = MARATHON_CEC_ALERT_PORT_BUSY;
        break;
    default:
        code = MARATHON_CEC_ALERT_OTHER;
        break;
    }
    cec->rust.alert(cec->rust.user, code);
}

static void on_source_activated(void *param, const cec_logical_address address,
                                const uint8_t activated)
{
    struct marathon_cec *cec = param;
    (void)address;
    cec->rust.source(cec->rust.user, activated ? 1 : 0);
}

static void clear_addresses(cec_logical_addresses *addresses)
{
    addresses->primary = CECDEVICE_UNREGISTERED;
    memset(addresses->addresses, 0, sizeof addresses->addresses);
}

static void set_error(char *error, size_t error_len, const char *format, ...)
{
    va_list args;
    if (error == NULL || error_len == 0) {
        return;
    }
    va_start(args, format);
    vsnprintf(error, error_len, format, args);
    va_end(args);
}

/*
 * Connects to a CEC adapter: `port` if given, otherwise the first one
 * found, waiting at most `timeout_ms` (0: libcec's default). `hdmi_port`
 * (1-15) overrides the HDMI input libcec would detect; 0 leaves it to
 * libcec. Returns NULL and fills `error` on failure.
 */
void *marathon_cec_open(const char *osd_name, int claim_on_start, int hdmi_port, const char *port,
                        uint32_t timeout_ms, const struct marathon_cec_callbacks *callbacks,
                        char *error, size_t error_len)
{
    struct marathon_cec *cec = calloc(1, sizeof *cec);
    char path[sizeof(((cec_adapter_descriptor *)0)->strComName)];
    if (cec == NULL) {
        set_error(error, error_len, "out of memory");
        return NULL;
    }
    cec->rust = *callbacks;

    cec->callbacks.logMessage = on_log;
    cec->callbacks.keyPress = on_key;
    cec->callbacks.commandReceived = on_command;
    cec->callbacks.alert = on_alert;
    cec->callbacks.sourceActivated = on_source_activated;

    libcec_clear_configuration(&cec->config);
    cec->config.clientVersion = LIBCEC_VERSION_CURRENT;
    snprintf(cec->config.strDeviceName, sizeof cec->config.strDeviceName, "%s", osd_name);
    cec->config.deviceTypes.types[0] = CEC_DEVICE_TYPE_PLAYBACK_DEVICE;
    /* Polite: no claiming the TV unless asked, no powering anything on or
     * off, and no shutting this machine down with the TV. */
    cec->config.bActivateSource = claim_on_start ? 1 : 0;
    cec->config.bPowerOffOnStandby = 0;
    clear_addresses(&cec->config.wakeDevices);
    clear_addresses(&cec->config.powerOffDevices);
    /* By default libcec holds Stop back as the start of a "combo key";
     * Marathon has no combos, so deliver every key at once. */
    cec->config.comboKey = CEC_USER_CONTROL_CODE_UNKNOWN;
    if (hdmi_port > 0) {
        cec->config.iHDMIPort = (uint8_t)hdmi_port;
        cec->config.baseDevice = CECDEVICE_TV;
    }
    cec->config.callbacks = &cec->callbacks;
    cec->config.callbackParam = cec;

    cec->connection = libcec_initialise(&cec->config);
    if (cec->connection == NULL) {
        set_error(error, error_len, "cannot initialise libcec");
        free(cec);
        return NULL;
    }
    /* Needed on Raspberry Pis using the firmware CEC driver; a no-op
     * elsewhere. */
    libcec_init_video_standalone(cec->connection);

    if (port == NULL || port[0] == '\0') {
        cec_adapter_descriptor found[1];
        int8_t count = libcec_detect_adapters(cec->connection, found, 1, NULL, 1);
        if (count <= 0) {
            set_error(error, error_len, "no CEC adapter found");
            libcec_destroy(cec->connection);
            free(cec);
            return NULL;
        }
        snprintf(path, sizeof path, "%s", found[0].strComName);
        port = path;
    }
    if (!libcec_open(cec->connection, port,
                     timeout_ms > 0 ? timeout_ms : CEC_DEFAULT_CONNECT_TIMEOUT)) {
        set_error(error, error_len, "cannot open the CEC adapter at %s", port);
        libcec_destroy(cec->connection);
        free(cec);
        return NULL;
    }
    return cec;
}

/* Disconnects and frees what marathon_cec_open returned. No callbacks are
 * made once this returns. */
void marathon_cec_close(void *handle)
{
    struct marathon_cec *cec = handle;
    if (cec == NULL) {
        return;
    }
    libcec_close(cec->connection);
    libcec_destroy(cec->connection);
    free(cec);
}
