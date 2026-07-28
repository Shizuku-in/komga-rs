<template>
  <v-container fluid class="pa-6">
    <v-row>
      <v-col>
        <server-settings/>
      </v-col>
    </v-row>
    <v-row>
      <v-col>
        <server-management/>
      </v-col>
    </v-row>
  </v-container>
</template>

<script lang="ts">
import {defineComponent} from 'vue'
import ServerSettings from '@/views/ServerSettings.vue'
import ServerManagement from '@/views/ServerManagement.vue'

export default defineComponent({
  name: 'SettingsServer',
  components: {ServerManagement, ServerSettings},
})
</script>

<style scoped>

</style>
